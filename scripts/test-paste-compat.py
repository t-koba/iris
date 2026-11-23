#!/usr/bin/env python3
"""Regression guard for the Iris-owned `paste` compatibility facade.

Iris no longer vendors a modified copy of the unmaintained `paste` crate.
Dependents keep the `paste`, `item` and `expr` proc macros through a small
facade that re-exports maintained pastey 0.1.1. This is configuration
evidence; the Rust build checks that the re-exported macros actually compile.
"""

from pathlib import Path
import re
import unittest

root = Path(__file__).resolve().parents[1]
registry_source = "registry+https://github.com/rust-lang/crates.io-index"
pastey_checksum = "35fb2e5f958ec131621fdd531e9fc186ed768cbe395337403ae56c17a74c68ec"
facade = root / "crates/paste-compat"


def read(relative):
    return (root / relative).read_text(encoding="utf-8")


def patch_section():
    match = re.search(r"\[patch\.crates-io\]\n(.*?)(?=\n\[|\Z)", read("Cargo.toml"), re.S)
    return match.group(1) if match else ""


def package_block(lock_text, name):
    pattern = (
        r'\[\[package\]\]\nname = "' + re.escape(name) + r'"\n'
        r'(.*?)(?=\n\[\[package\]\]|\Z)'
    )
    match = re.search(pattern, lock_text, re.S)
    return match.group(0) if match else ""


class PasteFacade(unittest.TestCase):
    def test_modified_vendor_copy_is_removed(self):
        self.assertFalse((root / "vendor/paste").exists(), "modified vendor/paste copy is still present")
        for relative in ("Cargo.toml", "docs/architecture.md", ".gitattributes", ".github/workflows/ci.yml"):
            self.assertNotIn("vendor/paste", read(relative), f"{relative} still names vendor/paste")
        self.assertNotIn("vendor/paste", patch_section())
        # The two remaining vendored copies keep their existing path patches.
        for name, path in (("cryoglyph", "vendor/cryoglyph"), ("jpegxr", "vendor/jpegxr")):
            self.assertIn(f'{name} = {{ path = "{path}" }}', patch_section())
            self.assertTrue((root / path).is_dir(), f"{path} is missing")

    def test_facade_contains_no_third_party_source(self):
        files = {str(path.relative_to(facade)) for path in facade.rglob("*") if path.is_file()}
        self.assertEqual(files, {"Cargo.toml", "src/lib.rs"}, "facade contains copied third-party files")
        manifest = read("crates/paste-compat/Cargo.toml")
        for expected in ('name = "paste"', 'version = "1.0.15"', 'pastey = "0.1.1"'):
            self.assertIn(expected, manifest, f"{expected} is missing from the facade manifest")
        self.assertNotIn("proc-macro = true", manifest)  # A library crate can re-export proc macros.
        source = read("crates/paste-compat/src/lib.rs")
        self.assertIn("pub use pastey::{expr, item, paste};", source)
        self.assertNotIn("macro_rules!", source)

    def test_workspace_patch_and_lock_use_facade(self):
        manifest = read("Cargo.toml")
        self.assertIn('"crates/paste-compat"', manifest.split("[workspace]")[1].split("[workspace.dependencies]")[0])
        self.assertIn('paste = { path = "crates/paste-compat" }', patch_section())
        lock_text = read("Cargo.lock")
        paste_block = package_block(lock_text, "paste")
        self.assertIn('version = "1.0.15"', paste_block)
        self.assertNotIn("source =", paste_block)
        self.assertIn('"pastey"', paste_block)
        pastey_block = package_block(lock_text, "pastey")
        self.assertIn('version = "0.1.1"', pastey_block)
        self.assertIn(f'source = "{registry_source}"', pastey_block)
        self.assertIn(f'checksum = "{pastey_checksum}"', pastey_block)

    def test_guard_runs_in_ci(self):
        self.assertIn("python3 scripts/test-paste-compat.py", read(".github/workflows/ci.yml"))


if __name__ == "__main__":
    unittest.main()
