#!/usr/bin/env python3
"""Bounded offline contracts for final POSIX payload notice verification."""
from importlib.util import module_from_spec, spec_from_file_location
import hashlib
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True


def load(name, path):
    spec = spec_from_file_location(name, path)
    module = module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


verifier = load("posix_notice_verifier", ROOT / "scripts/verify-posix-notices.py")
staging = load("linux_ffmpeg_pins", ROOT / "scripts/linux-ffmpeg.py")

# Entirely synthetic notice bytes and digests for the payload-shape tests.
SYNTHETIC = {
    "synthetic-a.txt": b"synthetic notice A, not a release file\n",
    "synthetic-b.txt": b"synthetic notice B, not a release file\n",
}
CASES = (("linux", "dir"), ("linux", "tar.gz"), ("macos", "dir"),
         ("macos", "zip"), ("macos", "pkg"))
DOCS = {"linux": "Iris.AppDir/usr/share/doc/iris/ffmpeg",
        "macos": "Iris.app/Contents/Resources/ffmpeg"}
MISSING = "block-mit.txt"
ALTERED = "jpegxr-bsd-2-clause.txt"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def synthetic_pins():
    return tuple((name, "synthetic/" + name, sha256(data))
                 for name, data in SYNTHETIC.items())


def real_notices():
    """Checked-in notice sources, not release binaries, bound to the real pins."""
    result = {}
    for name, relative, expected in verifier.pinned_notices():
        data = (ROOT / relative).read_bytes()
        assert sha256(data) == expected, name
        result[name] = data
    return result


def build_payload(root, platform, kind, notices):
    """A tree, archive or expanded PKG payload holding only synthetic files."""
    tree = root / "payload"
    prefix = "expanded/Payload/" if kind == "pkg" else ""
    docs = tree / (prefix + DOCS[platform])
    docs.mkdir(parents=True)
    for name, data in notices.items():
        (docs / name).write_bytes(data)
    if kind in ("dir", "pkg"):
        return tree
    if kind == "tar.gz":
        archive = root / "Iris-linux-appdir.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(tree, arcname="Iris.AppDir")
        return archive
    archive = root / "Iris-macos.zip"
    with zipfile.ZipFile(archive, "w") as zf:
        for path in sorted(tree.rglob("*")):
            if path.is_file():
                zf.write(path, "Iris.app/" + path.relative_to(tree).as_posix())
    return archive


def run_cli(payload, platform):
    return subprocess.run(
        [sys.executable, str(ROOT / "scripts/verify-posix-notices.py"),
         "--platform", platform, str(payload)],
        capture_output=True, text=True, timeout=30, check=False,
    )


class PosixNoticeVerification(unittest.TestCase):
    def test_pins_come_from_the_single_staging_table(self):
        self.assertEqual(verifier.pinned_notices(), staging.RUST_NOTICES)
        source = (ROOT / "scripts/verify-posix-notices.py").read_text(encoding="utf-8")
        for _, _, expected in staging.RUST_NOTICES:
            self.assertNotIn(expected, source, "notice pin duplicated in verifier")

    def test_synthetic_payload_shapes_pass(self):
        for platform, kind in CASES:
            with self.subTest(platform=platform, kind=kind), tempfile.TemporaryDirectory() as tmp:
                payload = build_payload(Path(tmp), platform, kind, SYNTHETIC)
                with patch.object(verifier, "pinned_notices", return_value=synthetic_pins()):
                    self.assertEqual(verifier.verify(payload, platform), len(SYNTHETIC))

    def test_synthetic_missing_notice_fails(self):
        for platform, kind in CASES:
            with self.subTest(platform=platform, kind=kind), tempfile.TemporaryDirectory() as tmp:
                notices = dict(SYNTHETIC)
                del notices["synthetic-b.txt"]
                payload = build_payload(Path(tmp), platform, kind, notices)
                with patch.object(verifier, "pinned_notices", return_value=synthetic_pins()):
                    with self.assertRaisesRegex(ValueError, "synthetic-b\\.txt"):
                        verifier.verify(payload, platform)

    def test_synthetic_altered_notice_fails(self):
        for platform, kind in CASES:
            with self.subTest(platform=platform, kind=kind), tempfile.TemporaryDirectory() as tmp:
                notices = dict(SYNTHETIC)
                notices["synthetic-a.txt"] += b"synthetic alteration"
                payload = build_payload(Path(tmp), platform, kind, notices)
                with patch.object(verifier, "pinned_notices", return_value=synthetic_pins()):
                    with self.assertRaisesRegex(ValueError, "synthetic-a\\.txt"):
                        verifier.verify(payload, platform)

    def test_cli_uses_real_pins_on_tree_and_zip(self):
        notices = real_notices()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tree = build_payload(root, "linux", "dir", notices)
            archive = build_payload(root / "zip", "macos", "zip", notices)
            self.assertEqual(run_cli(tree, "linux").returncode, 0)
            self.assertEqual(run_cli(archive, "macos").returncode, 0)

            missing = dict(notices)
            del missing[MISSING]
            self.assertIn(MISSING, run_cli(build_payload(root / "missing", "linux", "dir", missing),
                                          "linux").stderr)
            altered = dict(notices)
            altered[ALTERED] += b"\nsynthetic alteration\n"
            self.assertIn(ALTERED, run_cli(build_payload(root / "altered", "macos", "dir", altered),
                                          "macos").stderr)

    def test_workflow_gates_are_wired(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        release = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
        self.assertIn("run: python3 scripts/test-posix-notices.py", ci)
        for command in (
            "python3 scripts/verify-posix-notices.py --platform linux dist/linux/Iris.AppDir",
            "python3 scripts/verify-posix-notices.py --platform linux dist/linux/Iris-linux-appdir.tar.gz",
            "python3 scripts/verify-posix-notices.py --platform macos dist/macos/Iris.app",
            "python3 scripts/verify-posix-notices.py --platform macos dist/macos/Iris-macos.zip",
            "pkgutil --expand-full dist/macos/Iris-macos.pkg",
        ):
            self.assertIn(command, release)


if __name__ == "__main__":
    unittest.main()
