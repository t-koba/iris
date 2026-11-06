"""Bounded offline fixtures, not upstream provenance or a Windows build."""
import importlib.util
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("bundle", ROOT / "source-bundle.py")
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)
PORT = b'''vcpkg_from_github(
    PATCHES
        z-first.patch # retained inline comment
        "a-second.patch"
)
'''


class BundleTests(unittest.TestCase):
    def test_archive_material_integrity_and_determinism(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo = root / "repo"
            repo.mkdir()
            def git(*args):
                return bundle.git(repo, *args)
            git("init", "-q")
            for name, data in {
                "upstream.c": b"pristine source\n",
                "ports/ffmpeg/portfile.cmake": PORT,
                "ports/ffmpeg/vcpkg.json": b'{}\n',
                "ports/ffmpeg/z-first.patch": b"first\n",
                "ports/ffmpeg/a-second.patch": b"second\n",
                bundle.HELPER: b"helper\n",
            }.items():
                bundle.put(repo, name, data)
            git("add", ".")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                "commit", "-qm", "Fixture")
            commit = git("rev-parse", "HEAD").decode().strip()
            overlay = root / "overlay"
            shutil.copytree(repo / "ports/ffmpeg", overlay)
            # A patched checkout must not leak into the pristine archive.
            (repo / "upstream.c").write_bytes(b"patched source\n")
            with patch.multiple(bundle, COMMIT=commit, BASELINE=commit, ARCHIVE=f"ffmpeg-{commit}.tar"):
                first, second = root / "first", root / "second"
                bundle.create(repo, repo, overlay, first)
                bundle.create(repo, repo, overlay, second)
                expected = bundle.record(first / "MANIFEST.json")
                self.assertEqual(expected, bundle.record(second / "MANIFEST.json"))
                with tarfile.open(first / bundle.ARCHIVE) as archive:
                    self.assertEqual(archive.extractfile(f"ffmpeg-{commit}/upstream.c").read(), b"pristine source\n")
                    self.assertEqual(archive.pax_headers["comment"], commit)
                staged = root / "app-doc/ffmpeg-source-bundle"
                bundle.stage(first, staged, expected)
                self.assertEqual(expected, bundle.record(staged / "MANIFEST.json"))
                manifest = json.loads((first / "MANIFEST.json").read_text())
                self.assertIn("# retained inline comment", manifest["patches"]["block"])
                self.assertEqual([Path(p).name for p in manifest["patches"]["ordered_files"]],
                                 ["z-first.patch", "a-second.patch"])
                # Hash/size mismatches and missing inputs must all stop staging.
                for name in [bundle.ARCHIVE, "baseline/ports/ffmpeg/z-first.patch",
                             "overlay/ffmpeg/portfile.cmake", "build-inputs/pins.json", "MANIFEST.json"]:
                    path = first / name
                    original = path.read_bytes()
                    for data in [None, original + b"corrupt"]:
                        if data is None:
                            path.unlink()
                        else:
                            path.write_bytes(data)
                        with self.assertRaises((ValueError, FileNotFoundError)):
                            bundle.stage(first, root / "must-not-stage", expected)
                        self.assertFalse((root / "must-not-stage").exists())
                        path.write_bytes(original)
                # Even an internally rehashed manifest cannot omit a required
                # archive/patch, or rewrite patch comments/order.
                for name in [bundle.ARCHIVE, manifest["patches"]["ordered_files"][0]]:
                    changed = json.loads(json.dumps(manifest))
                    del changed["files"][name]
                    (first / "MANIFEST.json").write_text(json.dumps(changed))
                    with self.assertRaises(ValueError):
                        bundle.verify(first, bundle.record(first / "MANIFEST.json"))
                changed = json.loads(json.dumps(manifest))
                changed["patches"]["ordered_files"].reverse()
                (first / "MANIFEST.json").write_text(json.dumps(changed))
                with self.assertRaises(ValueError):
                    bundle.verify(first, bundle.record(first / "MANIFEST.json"))

    def test_package_and_evidence_wiring(self):
        build = (ROOT / "build-windows-ffmpeg-prototype.ps1").read_text()
        stage = (ROOT / "stage-windows-prototype-app.ps1").read_text()
        workflow = (ROOT.parent.parent / ".github/workflows/windows-ffmpeg-prototype.yml").read_text()
        self.assertLess(build.index('source-bundle.py") create'), build.index('& $vcpkgExe install'))
        self.assertIn('source_bundle_manifest = $sourceBundleRecord', build)
        self.assertIn('$bundleRecord = $build.source_bundle_manifest', stage)
        self.assertIn('source-bundle.py") stage', stage)
        self.assertIn('(Join-Path $docDir "ffmpeg-source-bundle")', stage)
        self.assertIn('if ($LASTEXITCODE -ne 0) { throw "refusing to stage: source bundle', stage)
        self.assertIn('target/prototype-windows-app/doc/ffmpeg-source-bundle/**', workflow)
        self.assertIn('test-source-bundle.py', workflow)
        self.assertIn('Flags: recursesubdirs', (ROOT / "windows-prototype-installer.iss").read_text())


if __name__ == "__main__":
    unittest.main()
