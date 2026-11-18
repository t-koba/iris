#!/usr/bin/env python3
"""Registry-pin evidence for the Iris-owned JPEG 2000 input guard.

Iris no longer vendors or patches `openjpeg-sys`. This checks the dependency
route: no in-repo copy or path patch remains, and Cargo.lock pins the unmodified
crates.io crate. It also checks that the Rust TLM/SOT guard and its tests remain
wired into CI. This is configuration evidence, not execution evidence: it does
not establish that an advisory proof of concept was reproduced or that a
particular input is safe.
"""

from pathlib import Path
import hashlib
import os
import re
import unittest

root = Path(__file__).resolve().parents[1]
crate_version = "1.0.12"
crate_sha256 = "92682cb92e7b01e2c020cf8ec12f3374558924bb24621df1112d8a7c93d419ef"
registry_source = "registry+https://github.com/rust-lang/crates.io-index"


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


def cached_crate():
    homes = []
    cargo_home = os.environ.get("CARGO_HOME")
    if cargo_home:
        homes.append(Path(cargo_home))
    homes.append(Path.home() / ".cargo")
    for home in homes:
        for candidate in home.glob(f"registry/cache/*/openjpeg-sys-{crate_version}.crate"):
            if candidate.is_file():
                return candidate
    return None


class OpenJpegRegistryRoute(unittest.TestCase):
    def test_no_in_repo_copy_or_path_patch(self):
        self.assertFalse(
            (root / "vendor/openjpeg-sys").exists(),
            "vendored openjpeg-sys copy is still present",
        )
        self.assertFalse(
            (root / "scripts/test-openjpeg-backport.py").exists(),
            "old vendored-backport check is still present",
        )
        self.assertNotIn("openjpeg", patch_section().lower())

        manifests = [root / "Cargo.toml"]
        manifests.extend(sorted((root / "crates").glob("*/Cargo.toml")))
        manifests.extend(sorted((root / "vendor").glob("*/Cargo.toml")))
        for manifest in manifests:
            text = manifest.read_text(encoding="utf-8")
            self.assertIsNone(
                re.search(r'openjpeg-sys\s*=\s*\{[^}]*\bpath\s*=', text),
                f"{manifest.relative_to(root)} patches openjpeg-sys to a local path",
            )

    def test_lockfile_pins_unmodified_registry_crate(self):
        lock_text = read("Cargo.lock")
        sys_block = package_block(lock_text, "openjpeg-sys")
        self.assertIn(f'version = "{crate_version}"', sys_block)
        self.assertIn(f'source = "{registry_source}"', sys_block)
        self.assertIn(f'checksum = "{crate_sha256}"', sys_block)
        self.assertNotIn("path", sys_block)

        jpeg2k_block = package_block(lock_text, "jpeg2k")
        self.assertIn('version = "0.10.1"', jpeg2k_block)
        self.assertIn(f'source = "{registry_source}"', jpeg2k_block)
        self.assertIn('"openjpeg-sys"', jpeg2k_block)

    def test_cached_crate_matches_pinned_checksum(self):
        crate = cached_crate()
        if crate is None:
            self.skipTest("crate not cached locally; Cargo enforces the lock checksum")
        self.assertEqual(hashlib.sha256(crate.read_bytes()).hexdigest(), crate_sha256)

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
        self.assertIn("python3 scripts/test-openjpeg-unpatched.py", ci)
        self.assertNotIn("test-openjpeg-backport.py", ci)
        self.assertNotIn("vendor/openjpeg-sys", read(".gitattributes"))
        self.assertNotIn("vendor/openjpeg-sys", read("docs/architecture.md"))


if __name__ == "__main__":
    unittest.main()
