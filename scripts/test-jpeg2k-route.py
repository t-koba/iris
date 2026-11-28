#!/usr/bin/env python3
"""Registry-pin evidence for the Iris JPEG 2000 decoder route and input guard.

Iris decodes JPEG 2000 with the pure-Rust `openjpeg2-pure-rs` crate (`openjp2`);
`jpeg2k` remains only a dev-dependency for fixture generation. No C decoder is
vendored, patched or statically linked. This checks that every Cargo.lock
pinning either crate (including the detached Windows prototype workspace) pins
the unmodified crates.io crates by checksum, that no in-repo copy or path patch
remains, and that the Rust TLM/SOT guard and its tests remain wired into CI. It
also checks that the verbatim upstream BSD-2-Clause notice, the font notices,
the SPDX MIT notice for the pinned `block` fork and the verbatim license texts
of the linked vendored crates reach the Linux, macOS and Windows release notice
sets with unchanged bytes. This is
configuration evidence, not execution evidence: it does not establish that an
advisory proof of concept was reproduced or that a particular input is safe.
"""

from pathlib import Path
import hashlib
import importlib.util
import json
import os
import re
import tempfile
import unittest
from unittest import mock

root = Path(__file__).resolve().parents[1]
registry_source = "registry+https://github.com/rust-lang/crates.io-index"
pinned = {
    "jpeg2k": ("0.11.0", "c8e4eea66cd0363fb2048d457f732a5d8a1bbbb2451e8db0bff7c4860defcafb"),
    "openjp2": ("0.7.2", "c41f2738518a4f8a8c6a9816b1b70b37511bc6f9f04d4f72230326f6abf3d489"),
    "openjp2-ctypes": ("0.7.2", "fc7650425b4f7e4c514e4a6ccaa2c26612f935d0826d2ee5426635d3844a8361"),
    "openjpeg2-pure-rs": ("0.1.1", "70cbec9906e23f99197f321718ce1f7e2fccd455e45c5b3f7d2bf5d7b635f192"),
}
SPDX_MIT_BODY = """MIT License

Copyright (c) Steven Sheldon

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""


def read(relative):
    return (root / relative).read_text(encoding="utf-8")


def package_block(lock_text, name):
    pattern = (
        r'\[\[package\]\]\nname = "' + re.escape(name) + r'"\n'
        r'(.*?)(?=\n\[\[package\]\]|\Z)'
    )
    match = re.search(pattern, lock_text, re.S)
    return match.group(0) if match else ""


def patch_section():
    manifest = read("Cargo.toml")
    match = re.search(r"\[patch\.crates-io\]\n(.*?)(?=\n\[|\Z)", manifest, re.S)
    return match.group(1) if match else ""


def jpeg2k_lockfiles():
    """Every Cargo.lock pinning a JPEG 2000 decoder crate, including detached workspaces."""
    found = []
    for path in root.rglob("Cargo.lock"):
        if "target" in path.parts:
            continue
        text = path.read_text(encoding="utf-8")
        if 'name = "jpeg2k"' in text or 'name = "openjpeg2-pure-rs"' in text:
            found.append(path)
    return sorted(found)


def cached_crate(name, version):
    homes = []
    cargo_home = os.environ.get("CARGO_HOME")
    if cargo_home:
        homes.append(Path(cargo_home))
    homes.append(Path.home() / ".cargo")
    for home in homes:
        for candidate in home.glob(f"registry/cache/*/{name}-{version}.crate"):
            if candidate.is_file():
                return candidate
    return None


class Jpeg2kRegistryRoute(unittest.TestCase):
    def test_no_in_repo_copy_or_path_patch(self):
        for name in ("openjpeg-sys", "jpeg2k", "openjp2"):
            self.assertFalse(
                (root / "vendor" / name).exists(),
                f"vendored {name} copy is still present",
            )
        self.assertFalse(
            (root / "scripts/test-openjpeg-backport.py").exists(),
            "old vendored-backport check is still present",
        )
        patch = patch_section().lower()
        for name in ("openjpeg", "jpeg2k", "openjp2"):
            self.assertNotIn(name, patch)

        manifests = [root / "Cargo.toml"]
        manifests.extend(sorted((root / "crates").glob("*/Cargo.toml")))
        manifests.extend(sorted((root / "vendor").glob("*/Cargo.toml")))
        for manifest in manifests:
            text = manifest.read_text(encoding="utf-8")
            for name in ("openjpeg-sys", "jpeg2k", "openjp2"):
                self.assertIsNone(
                    re.search(rf'{name}\s*=\s*\{{[^}}]*\bpath\s*=', text),
                    f"{manifest.relative_to(root)} patches {name} to a local path",
                )

    def test_every_lockfile_pins_pure_rust_registry_crates(self):
        locks = jpeg2k_lockfiles()
        self.assertIn(root / "prototype/windows-ffmpeg/preview-smoke/Cargo.lock", locks,
                      "detached preview-smoke lockfile is not covered")
        self.assertGreaterEqual(len(locks), 4)
        for lock in locks:
            with self.subTest(lock=str(lock.relative_to(root))):
                lock_text = lock.read_text(encoding="utf-8")
                self.assertNotIn('name = "openjpeg-sys"', lock_text,
                                 "C decoder is still in the lockfile")
                self.assertIn('name = "openjpeg2-pure-rs"', lock_text,
                              "pure-Rust decoder is not pinned")
                for name, (version, sha256) in pinned.items():
                    block = package_block(lock_text, name)
                    if not block:
                        # Fixture-generation crates may be absent from detached workspaces.
                        self.assertNotEqual(name, "openjpeg2-pure-rs")
                        continue
                    self.assertIn(f'version = "{version}"', block, f"{name} version")
                    self.assertIn(f'source = "{registry_source}"', block, f"{name} source")
                    self.assertIn(f'checksum = "{sha256}"', block, f"{name} checksum")
                    self.assertNotIn("path", block, f"{name} path patch")

    def test_cached_crates_match_pinned_checksums(self):
        for name, (version, sha256) in pinned.items():
            crate = cached_crate(name, version)
            if crate is None:
                continue
            self.assertEqual(
                hashlib.sha256(crate.read_bytes()).hexdigest(), sha256, name
            )

    def test_rust_guard_and_ci_wiring_present(self):
        modern_image = read("crates/thumb-preview/src/modern_image.rs")
        for name in (
            "fn j2k_tlm_sot_consistent",
            "fn read_tlm",
            "fn jpeg2000_codestream",
            "fn tlm_sot_inconsistency_is_rejected_before_decode",
            "fn tlm_sot_consistent_pair_is_accepted",
            "fn tlm_absent_codestream_is_preserved",
        ):
            self.assertIn(name, modern_image, f"{name} is missing from the Rust guard")
        self.assertIn("if !j2k_tlm_sot_consistent(data)", modern_image)

        ci = read(".github/workflows/ci.yml")
        self.assertIn("JPEG 2000 decoder and registry guard regressions", ci)
        self.assertIn("python3 scripts/test-jpeg2k-route.py", ci)
        self.assertNotIn("test-openjpeg-backport.py", ci)
        self.assertNotIn("vendor/openjpeg-sys", read(".gitattributes"))
        self.assertNotIn("vendor/openjpeg-sys", read("docs/architecture.md"))


class ReleaseNoticeSet(unittest.TestCase):
    """The pinned notice bytes reach every shipped notice set."""

    name = "openjpeg2-pure-rs-license.txt"
    sha256 = "a6af136f3e15038a666b61f376612a07d9a4e48cb7c01adbf3e33b3f14ab49b6"
    vendored = "packaging/third-party-notices/openjpeg2-pure-rs-license.txt"
    notices = (
        (name, vendored, sha256),
        ("noto-sans-cjk-jp-OFL.txt",
         "packaging/third-party-notices/noto-sans-cjk-jp-OFL.txt",
         "1c05c68c34f9708415aada51f17e1b0092d2cea709bf4a94cd38114f9e73d7d9"),
        ("material-symbols-apache-2.0.txt",
         "packaging/third-party-notices/material-symbols-apache-2.0.txt",
         "58d1e17ffe5109a7ae296caafcadfdbe6a7d176f0bc4ab01e12a689b0499d8bd"),
        ("block-mit.txt", "packaging/third-party-notices/block-mit.txt",
         "3dbb61cc50fd61a7114cc3dc10ab58d981228790d50e4dba2d537f87839b1acb"),
        ("cryoglyph-mit.txt",
         "packaging/third-party-notices/cryoglyph-mit.txt",
         "23f18e03dc49df91622fe2a76176497404e46ced8a715d9d2b67a7446571cca3"),
        ("cryoglyph-apache-2.0.txt",
         "packaging/third-party-notices/cryoglyph-apache-2.0.txt",
         "a60eea817514531668d7e00765731449fe14d059d3249e0bc93b36de45f759f2"),
        ("cryoglyph-zlib.txt",
         "packaging/third-party-notices/cryoglyph-zlib.txt",
         "b3ec01ad0869c5c50937bc68780cd0bb44e235ad858dfa54a7a381dced9b115a"),
        ("jpegxr-bsd-2-clause.txt",
         "packaging/third-party-notices/jpegxr-bsd-2-clause.txt",
         "f7a19d880e5c4cd98701970ae9550775b66fce292e7946b4af6a15f4887151d6"),
    )
    # The four appended notices are verbatim copies of files in the linked
    # vendored crates; the original four keep their positions.
    vendored_sources = {
        "packaging/third-party-notices/cryoglyph-mit.txt": "vendor/cryoglyph/LICENSE-MIT",
        "packaging/third-party-notices/cryoglyph-apache-2.0.txt": "vendor/cryoglyph/LICENSE-APACHE",
        "packaging/third-party-notices/cryoglyph-zlib.txt": "vendor/cryoglyph/LICENSE-ZLIB",
        "packaging/third-party-notices/jpegxr-bsd-2-clause.txt": "vendor/jpegxr/license.md",
    }

    def module(self, relative):
        spec = importlib.util.spec_from_file_location(relative, root / relative)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_vendored_notice_is_the_pinned_upstream_license(self):
        data = (root / self.vendored).read_bytes()
        self.assertEqual(hashlib.sha256(data).hexdigest(), self.sha256)
        for line in (b"2-clauses \n", b"other third \n", b"(CNES), France \n"):
            self.assertIn(line, data, "upstream trailing space changed")
        self.assertTrue(data.endswith(b" */\n"), "upstream final newline changed")

    def test_vendored_font_notices_are_the_pinned_upstream_bytes(self):
        noto = (root / self.notices[1][1]).read_bytes()
        self.assertEqual(hashlib.sha256(noto).hexdigest(), self.notices[1][2])
        self.assertTrue(noto.startswith(
            b"Copyright 2014-2021 Adobe (http://www.adobe.com/), "
            b"with Reserved Font Name 'Source'\n"), "OFL copyright head changed")
        for marker in (b"SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007\n",
                       b"PERMISSION & CONDITIONS\n",
                       b"OTHER DEALINGS IN THE FONT SOFTWARE.\n"):
            self.assertIn(marker, noto, "full OFL 1.1 text changed")
        apache = (root / self.notices[2][1]).read_bytes()
        self.assertEqual(hashlib.sha256(apache).hexdigest(), self.notices[2][2])
        for marker in (b"                                 Apache License\n",
                       b"                           Version 2.0, January 2004\n",
                       b"   APPENDIX: How to apply the Apache License to your work.\n"):
            self.assertIn(marker, apache, "full Apache-2.0 text changed")

    def test_block_notice_is_spdx_mit_with_pinned_attribution(self):
        _, relative, sha256 = self.notices[3]
        data = (root / relative).read_bytes()
        self.assertEqual(hashlib.sha256(data).hexdigest(), sha256)
        self.assertTrue(data.endswith(SPDX_MIT_BODY.encode("utf-8")),
                        "SPDX MIT License text changed")
        text = data.decode("utf-8")
        for marker in ("canonical MIT License text from SPDX",
                       "https://spdx.org/licenses/MIT.html", "b88ec0f",
                       "license = \"MIT\"", "author Steven Sheldon",
                       "upstream ships a LICENSE file",
                       "not a verbatim upstream LICENSE", "copyright year is asserted"):
            self.assertIn(marker, text, "block notice provenance changed")
        self.assertNotRegex(text, r"\b(?:19|20)\d{2}\b", "invented copyright year")
        self.assertIn("Copyright (c) Steven Sheldon\n".encode("utf-8"), data)

    def test_linked_vendored_notices_are_verbatim_vendor_bytes(self):
        """The four staged notices match the linked vendored license files."""
        ffmpeg = self.module("scripts/linux-ffmpeg.py")
        bundle = self.module("scripts/windows-release-source-bundle.py")
        appended = self.notices[4:]
        self.assertEqual(len(appended), 4)
        self.assertEqual({relative for _, relative, _ in appended},
                         set(self.vendored_sources))
        for name, relative, sha256 in appended:
            with self.subTest(notice=name):
                expected = (root / self.vendored_sources[relative]).read_bytes()
                staged = (root / relative).read_bytes()
                self.assertEqual(staged, expected, "staged notice is not verbatim")
                self.assertEqual(hashlib.sha256(expected).hexdigest(), sha256)
                with tempfile.TemporaryDirectory() as temporary:
                    docs = Path(temporary)
                    ffmpeg.stage_rust_notices(docs)
                    self.assertEqual((docs / name).read_bytes(), expected)
                    ffmpeg.check_rust_notices(docs)
                members, inputs = {}, {}
                bundle.add_third_party_notices(members, inputs)
                self.assertEqual(members[name], expected)
                self.assertEqual(inputs["third-party-notices/" + name]["sha256"], sha256)

    def test_linked_vendored_notice_staging_refuses_changed_or_absent_source(self):
        """Staging fails closed when a vendored source notice changes or vanishes."""
        ffmpeg = self.module("scripts/linux-ffmpeg.py")
        with tempfile.TemporaryDirectory() as temporary:
            sources = Path(temporary) / "sources"
            docs = Path(temporary) / "docs"
            docs.mkdir()
            for _, relative, _ in ffmpeg.RUST_NOTICES:
                target = sources / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes((root / relative).read_bytes())
            with mock.patch.object(ffmpeg, "ROOT", sources):
                ffmpeg.stage_rust_notices(docs)
                for relative in self.vendored_sources:
                    path = sources / relative
                    original = path.read_bytes()
                    with self.subTest(source_absent=relative):
                        path.unlink()
                        with self.assertRaises(FileNotFoundError):
                            ffmpeg.stage_rust_notices(docs)
                        path.write_bytes(original)
                    with self.subTest(source_tampered=relative):
                        path.write_bytes(original + b"tampered")
                        with self.assertRaisesRegex(
                                ValueError, "Rust notice differs from pinned bytes"):
                            ffmpeg.stage_rust_notices(docs)
                        path.write_bytes(original)
                ffmpeg.stage_rust_notices(docs)
            for name, _, _ in self.notices[4:]:
                with self.subTest(staged_absent=name):
                    staged = docs / name
                    staged.unlink()
                    with self.assertRaisesRegex(ValueError, "missing staged Rust notice"):
                        ffmpeg.check_rust_notices(docs)
                    ffmpeg.stage_rust_notices(docs)

    def test_posix_runtime_notice_sets_stage_and_verify_exact_bytes(self):
        ffmpeg = self.module("scripts/linux-ffmpeg.py")
        source = read("scripts/macos-ffmpeg.py")
        self.assertIn("common.stage_rust_notices(docs)", source,
                      "macOS bundle does not stage the notice")
        self.assertEqual(source.count("common.check_rust_notices(docs)"), 2,
                         "macOS staging and final audit must both check the notice")
        self.assertEqual(ffmpeg.RUST_NOTICES, self.notices)
        with tempfile.TemporaryDirectory() as temporary:
            docs = Path(temporary)
            ffmpeg.stage_rust_notices(docs)
            for name, relative, _ in self.notices:
                with self.subTest(notice=name):
                    expected = (root / relative).read_bytes()
                    staged = docs / name
                    self.assertEqual(staged.read_bytes(), expected)
                    ffmpeg.check_rust_notices(docs)
                    staged.write_bytes(expected + b"tampered")
                    with self.assertRaisesRegex(ValueError, "staged Rust notice changed"):
                        ffmpeg.check_rust_notices(docs)
                    staged.write_bytes(expected)
            for name, _, _ in self.notices:
                with self.subTest(missing=name):
                    (docs / name).unlink()
                    with self.assertRaisesRegex(ValueError, "missing staged Rust notice"):
                        ffmpeg.check_rust_notices(docs)
                    ffmpeg.stage_rust_notices(docs)

    def test_windows_legal_notice_archive_carries_exact_bytes(self):
        bundle = self.module("scripts/windows-release-source-bundle.py")
        members, inputs = {}, {}
        bundle.add_third_party_notices(members, inputs)
        readme_notices = {item[0] for item in self.notices[:4]}
        for name, relative, sha256 in self.notices:
            with self.subTest(notice=name):
                self.assertIn((name, relative, sha256), bundle.THIRD_PARTY_NOTICES)
                expected = (root / relative).read_bytes()
                self.assertEqual(members[name], expected)
                binding = inputs["third-party-notices/" + name]
                self.assertEqual((binding["sha256"], binding["bytes"]),
                                 (sha256, len(expected)))
                if name in readme_notices:
                    self.assertIn(name, read("packaging/windows/ffmpeg-README.md"))
                else:
                    self.assertIn(name, read("docs/architecture.md"),
                                  "linked vendored notice is not described")

    def test_windows_legal_notice_archive_rejects_tampered_or_missing_bytes(self):
        bundle = self.module("scripts/windows-release-source-bundle.py")
        with tempfile.TemporaryDirectory() as temporary:
            stage = Path(temporary)
            for _, relative, _ in bundle.THIRD_PARTY_NOTICES:
                target = stage / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes((root / relative).read_bytes())
            with mock.patch.object(bundle, "ROOT", stage):
                for name, relative, _ in self.notices:
                    with self.subTest(tamper=name):
                        members, inputs = {}, {}
                        bundle.add_third_party_notices(members, inputs)
                        self.assertEqual(len(members), len(bundle.THIRD_PARTY_NOTICES))
                        path = stage / relative
                        original = path.read_bytes()
                        path.write_bytes(original + b"tampered")
                        with self.assertRaisesRegex(ValueError,
                                                   "third-party notice changed: " + name):
                            bundle.add_third_party_notices({}, {})
                        path.write_bytes(original)
                for name, relative, _ in self.notices:
                    with self.subTest(missing=name):
                        path = stage / relative
                        original = path.read_bytes()
                        path.unlink()
                        with self.assertRaisesRegex(ValueError, "absent or unsupported"):
                            bundle.add_third_party_notices({}, {})
                        path.write_bytes(original)

    def test_posix_native_evidence_maps_the_staged_notice(self):
        # Only the original four notices are listed in the reviewed native
        # evidence mapping; the appended vendored notices are staged as docs.
        suffixes = tuple(Path(relative).name for _, relative, _ in self.notices[:4])
        roots = {"linux": "Iris.AppDir/usr/share/doc/iris/ffmpeg/",
                 "macos": "Iris.app/Contents/Resources/ffmpeg/"}
        for platform, prefix in roots.items():
            mapping = json.loads(read(f"packaging/native-evidence/{platform}-mapping.json"))
            roles = [rule["role"] for rule in mapping["staged_rules"]
                     if rule["path"].endswith("/ffmpeg/*")]
            self.assertEqual(roles, ["evidence"])
            for name in suffixes:
                with self.subTest(platform=platform, notice=name):
                    self.assertIn(prefix + name, mapping["evidence_files"])
                    self.assertIn(name, read(f"packaging/{platform}/ffmpeg-README.md"))


if __name__ == "__main__":
    unittest.main()
