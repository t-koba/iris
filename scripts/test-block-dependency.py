#!/usr/bin/env python3
"""Regression guard for the pinned public `block` dependency.

Iris no longer carries a modified `vendor/block` copy. metal-rs still needs
block 0.1.6, whose upstream release declares an uninhabited FFI marker; a
public MIT-licensed fork keeps the same API/ABI with an inhabited marker and
is pinned by commit. This check binds the manifest and lock to that seam.
"""

from pathlib import Path
import re
import unittest

root = Path(__file__).resolve().parents[1]
fork_url = "https://github.com/vaimeai/rust-block"
fork_rev = "b88ec0f"
fork_commit = "b88ec0fad1ed2a675bef651e8e23d49b649ba0be"


def read(relative):
    return (root / relative).read_text(encoding="utf-8")


class BlockDependency(unittest.TestCase):
    def test_in_repo_copy_is_removed(self):
        self.assertFalse((root / "vendor/block").exists(), "modified vendor/block copy is still present")
        for relative in ("Cargo.toml", "docs/architecture.md", ".gitattributes", ".github/workflows/ci.yml"):
            self.assertNotIn("vendor/block", read(relative), f"{relative} still names vendor/block")

    def test_manifest_uses_pinned_public_fork(self):
        patch = re.search(r"\[patch\.crates-io\]\n(.*?)(?=\n\[|\Z)", read("Cargo.toml"), re.S).group(1)
        entry = next(line for line in patch.splitlines() if line.startswith("block = "))
        self.assertIn(f'git = "{fork_url}"', entry)
        self.assertIn(f'rev = "{fork_rev}"', entry)
        self.assertNotIn("path =", entry)
        self.assertIn("MIT", patch)

    def test_lock_pins_the_fork_commit(self):
        lock = read("Cargo.lock")
        package = re.search(r'\[\[package\]\]\nname = "block"\n(.*?)(?=\n\[\[package\]\]|\Z)', lock, re.S).group(1)
        self.assertIn(f"git+{fork_url}?rev={fork_rev}#{fork_commit}", package)

    def test_guard_runs_in_ci(self):
        self.assertIn("python3 scripts/test-block-dependency.py", read(".github/workflows/ci.yml"))


if __name__ == "__main__":
    unittest.main()
