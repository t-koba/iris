#!/usr/bin/env python3
"""Registry-pin evidence for the Iris JPEG 2000 decoder route and input guard.

Iris decodes JPEG 2000 with the pure-Rust `openjpeg2-pure-rs` crate (`openjp2`);
`jpeg2k` remains only a dev-dependency for fixture generation. No C decoder is
vendored, patched or statically linked. This checks that every Cargo.lock
pinning either crate (including the detached Windows prototype workspace) pins
the unmodified crates.io crates by checksum, that no in-repo copy or path patch
remains, and that the Rust TLM/SOT guard and its tests remain wired into CI. It
also checks that the verbatim upstream BSD-2-Clause notice reaches the Linux,
macOS and Windows release notice sets with unchanged bytes. This is
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

root = Path(__file__).resolve().parents[1]
registry_source = "registry+https://github.com/rust-lang/crates.io-index"
pinned = {
    "jpeg2k": ("0.11.0", "c8e4eea66cd0363fb2048d457f732a5d8a1bbbb2451e8db0bff7c4860defcafb"),
    "openjp2": ("0.7.2", "c41f2738518a4f8a8c6a9816b1b70b37511bc6f9f04d4f72230326f6abf3d489"),
    "openjp2-ctypes": ("0.7.2", "fc7650425b4f7e4c514e4a6ccaa2c26612f935d0826d2ee5426635d3844a8361"),
    "openjpeg2-pure-rs": ("0.1.1", "70cbec9906e23f99197f321718ce1f7e2fccd455e45c5b3f7d2bf5d7b635f192"),
}


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
    """The upstream BSD-2-Clause bytes reach every shipped notice set."""

    name = "openjpeg2-pure-rs-license.txt"
    sha256 = "a6af136f3e15038a666b61f376612a07d9a4e48cb7c01adbf3e33b3f14ab49b6"
    vendored = "packaging/third-party-notices/openjpeg2-pure-rs-license.txt"

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

    def test_posix_runtime_notice_sets_stage_and_verify_exact_bytes(self):
        ffmpeg = self.module("scripts/linux-ffmpeg.py")
        source = read("scripts/macos-ffmpeg.py")
        self.assertIn("common.stage_rust_notices(docs)", source,
                      "macOS bundle does not stage the notice")
        self.assertEqual(source.count("common.check_rust_notices(docs)"), 2,
                         "macOS staging and final audit must both check the notice")
        self.assertEqual(ffmpeg.RUST_NOTICES,
                         ((self.name, self.vendored, self.sha256),))
        expected = (root / self.vendored).read_bytes()
        with tempfile.TemporaryDirectory() as temporary:
            docs = Path(temporary)
            ffmpeg.stage_rust_notices(docs)
            staged = docs / self.name
            self.assertEqual(staged.read_bytes(), expected)
            ffmpeg.check_rust_notices(docs)
            staged.write_bytes(expected + b"tampered")
            with self.assertRaisesRegex(ValueError, "staged Rust notice changed"):
                ffmpeg.check_rust_notices(docs)
            staged.unlink()
            with self.assertRaisesRegex(ValueError, "missing staged Rust notice"):
                ffmpeg.check_rust_notices(docs)

    def test_windows_legal_notice_archive_carries_exact_bytes(self):
        bundle = self.module("scripts/windows-release-source-bundle.py")
        self.assertIn((self.name, self.vendored, self.sha256),
                      bundle.THIRD_PARTY_NOTICES)
        members, inputs = {}, {}
        bundle.add_third_party_notices(members, inputs)
        expected = (root / self.vendored).read_bytes()
        self.assertEqual(members[self.name], expected)
        binding = inputs["third-party-notices/" + self.name]
        self.assertEqual((binding["sha256"], binding["bytes"]),
                         (self.sha256, len(expected)))
        self.assertIn(self.name, read("packaging/windows/ffmpeg-README.md"))

    def test_posix_native_evidence_maps_the_staged_notice(self):
        staged = {
            "linux": "Iris.AppDir/usr/share/doc/iris/ffmpeg/" + self.name,
            "macos": "Iris.app/Contents/Resources/ffmpeg/" + self.name,
        }
        for platform, path in staged.items():
            with self.subTest(platform=platform):
                mapping = json.loads(read(f"packaging/native-evidence/{platform}-mapping.json"))
                self.assertIn(path, mapping["evidence_files"])
                roles = [rule["role"] for rule in mapping["staged_rules"]
                         if rule["path"].endswith("/ffmpeg/*")]
                self.assertEqual(roles, ["evidence"])
                self.assertIn(self.name, read(f"packaging/{platform}/ffmpeg-README.md"))


if __name__ == "__main__":
    unittest.main()
