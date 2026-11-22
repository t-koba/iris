#!/usr/bin/env python3
"""Offline contracts for the Windows release source/notice evidence bundle."""

import argparse
import contextlib
import hashlib
import io
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]


def load():
    spec = importlib.util.spec_from_file_location(
        "bundle", ROOT / "scripts/windows-release-source-bundle.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SourceBundleContracts(unittest.TestCase):
    def setUp(self):
        self.mod = load()
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        base = Path(self.tmp.name)
        self.pin = dict(self.mod.source.source_pin())
        self.source_bytes = b"synthetic verified ffmpeg source archive"
        self.pin["sha256"] = hashlib.sha256(self.source_bytes).hexdigest()
        real_pdfium = self.mod.source.pdfium_pin()
        self.pdfium = dict(real_pdfium)
        self.pdfium_bytes = b"synthetic attested pdfium asset"
        self.pdfium["bytes"] = len(self.pdfium_bytes)
        self.pdfium["sha256"] = hashlib.sha256(self.pdfium_bytes).hexdigest()
        for name, value in (("source_pin", lambda: self.pin),
                            ("pdfium_pin", lambda: self.pdfium)):
            patcher = mock.patch.object(self.mod.source, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.archive = base / "ffmpeg-8.1.3.tar.xz"
        self.archive.write_bytes(self.source_bytes)
        self.tree = base / "source" / "ffmpeg-8.1.3"
        self.tree.mkdir(parents=True)
        (self.tree / "configure").write_text("#!/bin/sh\n", newline="\n")
        (self.tree / "RELEASE").write_text("8.1.3\n", newline="\n")
        self.legal = {
            "COPYING.LGPLv2.1": "LGPL 2.1 text\n",
            "COPYING.GPLv2": "GPL 2 text\n",
            "COPYRIGHT": "FFmpeg developers\n",
            "CREDITS": "FFmpeg credits\n",
        }
        for name, value in self.legal.items():
            (self.tree / name).write_text(value, newline="\n")
        acquisition = {
            "policy": {"sha256": self.pin["sha256"], "url": self.pin["url"]},
            "signature_verified": True,
            "source_sha256": self.pin["sha256"],
            "files": {
                "ffmpeg-8.1.3/" + name: hashlib.sha256(value.encode()).hexdigest()
                for name, value in self.legal.items()
            },
        }
        # windows-ffmpeg-source.py stages the manifest beside the extracted
        # ffmpeg-<version> directory, not inside it.
        (self.tree.parent / "acquisition.json").write_text(
            json.dumps(acquisition, indent=2), newline="\n")
        self.gpg = base / "gpg"
        self.gpg.mkdir()
        for name in ("ffmpeg-8.1.3.tar.xz.asc", "ffmpeg-key.asc", "status.txt"):
            (self.gpg / name).write_text("[GNUPG:] VALIDSIG mock\n", newline="\n")
        self.overlay = base / "overlay" / "ffmpeg"
        self.overlay.mkdir(parents=True)
        (self.overlay / "portfile.cmake").write_text(
            'set(SOURCE_PATH "%s")\n' % self.tree, newline="\n")
        (self.overlay / "portfile.evidence.json").write_text(
            json.dumps({"source_path": str(self.tree), "baseline_patches": []}), newline="\n")
        (self.overlay / "vcpkg.json").write_text('{"name":"ffmpeg","version":"8.1.3"}\n', newline="\n")
        (self.overlay / "usage").write_text("ffmpeg needs nasm\n", newline="\n")
        patches = self.overlay / "patches"
        patches.mkdir()
        for name in self.mod.overlay.baseline_patches():
            (patches / name).write_text("patch " + name + "\n", newline="\n")
        self.pdf_asset = base / "pdfium-win-x64.tgz"
        self.pdf_asset.write_bytes(self.pdfium_bytes)
        self.extract = base / "pdfium-extract"
        (self.extract / "licenses").mkdir(parents=True)
        (self.extract / "LICENSE").write_text("PDFium BSD license\n", newline="\n")
        (self.extract / "VERSION").write_text("MAJOR=159\nBUILD=8138\n", newline="\n")
        (self.extract / "args.gn").write_text("is_official_build = true\n", newline="\n")
        (self.extract / "licenses" / "pdfium.txt").write_text("third party\n", newline="\n")
        (self.extract / "licenses" / "extra.txt").write_text("more notices\n", newline="\n")
        self.stage = base / "stage"
        self.stage.mkdir()
        self.report = base / "evidence" / "ffmpeg-source-bundle.json"
        # Bind the synthetic fixture notice to the same synthetic pin, while
        # keeping the real checked-in wording and replacement instructions.
        self.notice = base / "FFMPEG-NOTICE.md"
        self.notice.write_text((ROOT / "packaging/windows/ffmpeg-README.md").read_text()
                               .replace(real_pdfium["sha256"], self.pdfium["sha256"]), newline="\n")
        self.args = argparse.Namespace(
            source_archive=self.archive, source_tree=self.tree, gpg_dir=self.gpg,
            overlay=self.overlay.parent, pdfium_asset=self.pdf_asset,
            pdfium_extract=self.extract, notice=self.notice,
            output=self.stage, report=self.report)

    def build(self, args=None):
        with contextlib.redirect_stdout(io.StringIO()):
            self.mod.build(args or self.args)
        return json.loads(self.report.read_text())

    def members(self, name):
        with tarfile.open(self.stage / name, "r:gz") as archive:
            return {member.name: archive.extractfile(member).read()
                    for member in archive.getmembers()}

    def test_plan_is_bounded_and_names_outputs(self):
        result = subprocess.run(
            [sys.executable, str(ROOT / "scripts/windows-release-source-bundle.py"), "plan"],
            capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        shape = json.loads(result.stdout)
        self.assertEqual(shape["version"], "8.1.3")
        self.assertEqual(shape["outputs"], list(self.mod.OUTPUTS))
        self.assertEqual(shape["legal_files"],
                         list(self.mod.REQUIRED_LEGAL_FILES + self.mod.COPYRIGHT_FILES))
        self.assertIn("licenses/pdfium.txt", shape["pdfium_files"])
        self.assertTrue(all(shape["bounds"][key] > 0 for key in shape["bounds"]))

    def test_bundles_are_deterministic_and_hash_every_input(self):
        report = self.build()
        for name in self.mod.OUTPUTS:
            self.assertTrue((self.stage / name).is_file(), name)
        self.assertEqual((self.stage / "ffmpeg-8.1.3.tar.xz").read_bytes(), self.source_bytes)
        self.assertEqual(report["outputs"]["ffmpeg-8.1.3.tar.xz"]["sha256"],
                         hashlib.sha256(self.source_bytes).hexdigest())
        legal = self.members("ffmpeg-legal-notices.tar.gz")
        for name in self.mod.REQUIRED_LEGAL_FILES + ("COPYRIGHT",):
            self.assertEqual(legal[name].decode(), self.legal[name])
        self.assertEqual(legal["FFMPEG-NOTICE.md"], self.notice.read_bytes())
        section = " ".join(legal["REPLACEMENT.md"].decode().lower().split())
        self.assertIn("swap the versioned dll set in place", section)
        materials = self.members("ffmpeg-build-materials.tar.gz")
        self.assertIn(self.mod.SOURCE_TOKEN.encode(), materials["overlay/ffmpeg/portfile.cmake"])
        self.assertNotIn(str(self.tree).encode(), materials["overlay/ffmpeg/portfile.cmake"])
        self.assertIn("source-acquisition.json", materials)
        for name in self.mod.overlay.baseline_patches():
            self.assertIn("overlay/ffmpeg/patches/" + name, materials)
        for name in ("signature/ffmpeg-8.1.3.tar.xz.asc", "signature/ffmpeg-key.asc",
                     "signature/gpg-status.txt", "vcpkg.json",
                     "vcpkg-configuration.json", "triplets/x64-windows-iris-dynamic.cmake"):
            self.assertIn(name, materials)
        pdfium = self.members("pdfium-notices.tar.gz")
        self.assertEqual(set(pdfium), {"LICENSE", "VERSION", "args.gn",
                                       "licenses/pdfium.txt", "licenses/extra.txt"})
        for name in ("ffmpeg-source-archive", "ffmpeg-notice", "pdfium-asset",
                     "pdfium/VERSION", "source-acquisition", "signature/ffmpeg-key.asc",
                     "archive:pdfium-notices.tar.gz"):
            self.assertIn(name, report["inputs"])
        self.assertEqual(report["inputs"]["ffmpeg-source/COPYRIGHT"]["sha256"],
                         hashlib.sha256(self.legal["COPYRIGHT"].encode()).hexdigest())
        self.assertEqual(json.loads((self.stage / "ffmpeg-source-bundle.json").read_text()),
                         report)
        second = Path(self.tmp.name) / "second"
        second.mkdir()
        self.args.output = second
        self.build()
        for name in self.mod.OUTPUTS:
            self.assertEqual((self.stage / name).read_bytes(), (second / name).read_bytes(),
                             name)

    def test_source_hash_must_match_pin(self):
        self.archive.write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "signature-verified pin"):
            self.build()

    def test_requires_copyright_text_from_verified_source(self):
        for name in self.mod.COPYRIGHT_FILES:
            (self.tree / name).unlink(missing_ok=True)
        with self.assertRaisesRegex(ValueError, "lacks upstream copyright"):
            self.build()

    def test_requires_acquisition_at_staging_root(self):
        root_record = self.tree.parent / "acquisition.json"
        misplaced = root_record.read_bytes()
        root_record.unlink()
        (self.tree / "acquisition.json").write_bytes(misplaced)
        with self.assertRaisesRegex(ValueError, "source acquisition absent or unsupported"):
            self.build()

    def test_rejects_unsupported_acquisition_digest_shape(self):
        record = json.loads((self.tree.parent / "acquisition.json").read_text())
        member = "ffmpeg-8.1.3/COPYING.LGPLv2.1"
        record["files"][member] = {"sha256": record["files"][member]}
        (self.tree.parent / "acquisition.json").write_text(
            json.dumps(record, indent=2), newline="\n")
        with self.assertRaisesRegex(ValueError, "digest for COPYING.LGPLv2.1 is unsupported"):
            self.build()

    def test_rejects_legal_text_drifted_from_acquisition(self):
        (self.tree / "COPYING.LGPLv2.1").write_text("changed license\n", newline="\n")
        with self.assertRaisesRegex(ValueError, "differs from the verified source"):
            self.build()

    def test_rejects_notice_without_replacement_instructions(self):
        damaged = self.notice.parent / "notice.tmp"
        damaged.write_text(self.notice.read_text()
                           .replace("swap the versioned DLL set", "replace the DLL set"), newline="\n")
        self.args.notice = damaged
        with self.assertRaisesRegex(ValueError, "replacement section lacks marker"):
            self.build()

    def test_rejects_symbolic_link_in_pdfium_notices(self):
        (self.extract / "licenses" / "link.txt").symlink_to(self.extract / "LICENSE")
        with self.assertRaisesRegex(ValueError, "symlinked PDFium license"):
            self.build()

    def test_rejects_unexpected_overlay_file(self):
        (self.overlay / "unexpected.txt").write_text("extra\n", newline="\n")
        with self.assertRaisesRegex(ValueError, "unexpected file set"):
            self.build()

    def test_rejects_unsafe_patch_name(self):
        patches = self.overlay / "patches"
        (patches / "..evil.patch").write_text("..evil\n", newline="\n")
        with self.assertRaisesRegex(ValueError, "unsafe evidence path"):
            self.build()

    def test_rejects_oversized_member(self):
        (self.tree / "COPYRIGHT").write_bytes(b"x" * (self.mod.MAX_MEMBER_BYTES + 1))
        with self.assertRaisesRegex(ValueError, "exceeds member bound"):
            self.build()

    def test_rejects_pdfium_asset_drift(self):
        self.pdf_asset.write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "attested pin"):
            self.build()


if __name__ == "__main__":
    unittest.main()
