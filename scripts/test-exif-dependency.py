#!/usr/bin/env python3
"""Guard the exact kamadak-exif pin against GHSA-53gf-vqf6-qvp8.

0.6.1 and older revisit already-parsed IFDs and can allocate O(n^3) fields
from a small crafted image. Iris parses untrusted JPEG/Exif bytes and builds
with panic=abort, so the pin and every detached lockfile are release inputs.
"""

from pathlib import Path
import re
import unittest

root = Path(__file__).resolve().parents[1]
version = "0.6.2"
checksum = "8772c92c0f41c4a672cbdee6728253e5ae8fcc1c5eb5c6c676579565684bbf68"
lockfiles = (
    "Cargo.lock",
    "crates/thumb-preview/fuzz/Cargo.lock",
    "crates/ui-shell/fuzz/Cargo.lock",
    "prototype/windows-ffmpeg/preview-smoke/Cargo.lock",
)


def package_block(text):
    match = re.search(
        r'\[\[package\]\]\nname = "kamadak-exif"\n(.*?)(?=\n\[\[package\]\]|\Z)',
        text,
        re.S,
    )
    return match.group(1) if match else ""


class ExifDependency(unittest.TestCase):
    def test_workspace_pin_is_exact(self):
        manifest = (root / "Cargo.toml").read_text(encoding="utf-8")
        self.assertIn('kamadak-exif = "=0.6.2"', manifest)
        self.assertNotIn('kamadak-exif = "0.6"', manifest)

    def test_all_lockfiles_use_patched_version(self):
        for relative in lockfiles:
            block = package_block((root / relative).read_text(encoding="utf-8"))
            self.assertIn(f'version = "{version}"', block, f"{relative} has the wrong Exif version")
            self.assertIn(f'checksum = "{checksum}"', block, f"{relative} has the wrong Exif checksum")
            self.assertNotIn('version = "0.6.1"', block, f"{relative} still pins the vulnerable Exif")

    def test_guard_runs_in_ci(self):
        ci = (root / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        self.assertIn("run: python3 scripts/test-exif-dependency.py", ci)


if __name__ == "__main__":
    unittest.main()
