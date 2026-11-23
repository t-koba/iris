#!/usr/bin/env python3
"""Registry-pin evidence for the Iris JPEG 2000 decoder route and input guard.

Iris decodes JPEG 2000 with the pure-Rust `jpeg2k` / `openjp2` crates; no C
decoder is vendored, patched or statically linked. This checks that every
Cargo.lock pinning jpeg2k (including the detached Windows prototype workspace)
pins the unmodified crates.io crates by checksum, that no in-repo copy or path
patch remains, and that the Rust TLM/SOT guard and its tests remain wired into
CI. This is configuration evidence, not execution evidence: it does not
establish that an advisory proof of concept was reproduced or that a particular
input is safe.
"""

from pathlib import Path
import hashlib
import os
import re
import unittest

root = Path(__file__).resolve().parents[1]
registry_source = "registry+https://github.com/rust-lang/crates.io-index"
pinned = {
    "jpeg2k": ("0.11.0", "c8e4eea66cd0363fb2048d457f732a5d8a1bbbb2451e8db0bff7c4860defcafb"),
    "openjp2": ("0.7.2", "c41f2738518a4f8a8c6a9816b1b70b37511bc6f9f04d4f72230326f6abf3d489"),
    "openjp2-ctypes": ("0.7.2", "fc7650425b4f7e4c514e4a6ccaa2c26612f935d0826d2ee5426635d3844a8361"),
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
    """Every Cargo.lock pinning jpeg2k, including detached prototype workspaces."""
    found = []
    for path in root.rglob("Cargo.lock"):
        if "target" in path.parts:
            continue
        if 'name = "jpeg2k"' in path.read_text(encoding="utf-8"):
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
                for name, (version, sha256) in pinned.items():
                    block = package_block(lock_text, name)
                    self.assertIn(f'version = "{version}"', block, f"{name} version")
                    self.assertIn(f'source = "{registry_source}"', block, f"{name} source")
                    self.assertIn(f'checksum = "{sha256}"', block, f"{name} checksum")
                    self.assertNotIn("path", block, f"{name} path patch")
                self.assertIn('"openjp2"', package_block(lock_text, "jpeg2k"))

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


if __name__ == "__main__":
    unittest.main()
