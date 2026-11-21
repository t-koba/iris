#!/usr/bin/env python3
"""Offline text contract for the fail-closed Windows product ZIP rehearsal."""

from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/windows-product-package.ps1"

REQUIRED = (
    '$ErrorActionPreference = "Stop"',
    "IRIS_PRODUCT_STAGE",
    "IRIS_PRODUCT_INVENTORY",
    "IRIS_PRODUCT_ZIP",
    'throw "missing IRIS_PRODUCT_STAGE"',
    'throw "missing IRIS_PRODUCT_INVENTORY"',
    'throw "missing IRIS_PRODUCT_ZIP"',
    'throw "product inventory has no files"',
    'throw "product inventory entry count out of bounds"',
    'throw "product inventory file missing from stage: $($item.name)"',
    'throw "product file byte mismatch: $($item.name)"',
    'throw "product file sha256 mismatch: $($item.name)"',
    'throw "unlisted file in product stage: $($file.Name)"',
    'throw "required product file missing from inventory: $required"',
    'throw "unsafe product inventory name: $name"',
    'throw "duplicate product inventory name: $name"',
    "Get-ChildItem -LiteralPath $stage -File",
    "if ($staged.Count -gt 256) { throw \"stage listing exceeds bound\" }",
    "if ($entries.Count -eq 0 -or $entries.Count -gt 256)",
    "Compress-Archive -LiteralPath $sources -DestinationPath $zipPath",
    'throw "product package output must be outside the stage"',
    "zip_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $zipPath)",
    'entries = @($ordered)',
    'Set-Content -LiteralPath $resultPath',
    "-LiteralPath",
)

FORBIDDEN = (
    "cargo", "inno", "iscc", "invoke-webrequest", "curl", "wget",
    "http://", "https://", "build-variant", "build_variant",
    "ir_build_variant", "release.yml", "gh release", "github",
)


def contract_holds(text):
    return all(marker in text for marker in REQUIRED)


class ProductPackageContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = SCRIPT.read_text(encoding="utf-8")
        cls.lower = cls.text.lower()

    def test_script_validates_under_offline_contract(self):
        self.assertTrue(contract_holds(self.text))

    def test_every_marker_is_fail_closed(self):
        for marker in REQUIRED:
            with self.subTest(marker=marker):
                self.assertIn(marker, self.text)
                self.assertFalse(contract_holds(self.text.replace(marker, "<removed>")))

    def test_bound_inputs(self):
        for name in ("IRIS_PRODUCT_STAGE", "IRIS_PRODUCT_INVENTORY", "IRIS_PRODUCT_ZIP"):
            with self.subTest(name=name):
                self.assertIn(f'$env:{name}', self.text)
                self.assertIn(f'throw "missing {name}"', self.text)

    def test_inventory_identity_and_missing_file_guards(self):
        for marker in (
            "Get-Content -LiteralPath $inventoryPath -Raw | ConvertFrom-Json",
            'throw "product inventory has no files"',
            'throw "product inventory entry count out of bounds"',
            'throw "product inventory file missing from stage: $($item.name)"',
            'throw "required product file missing from inventory: $required"',
            "Test-Path -LiteralPath $path -PathType Leaf",
        ):
            with self.subTest(marker=marker):
                self.assertIn(marker, self.text)

    def test_hash_and_byte_mismatch_guards(self):
        self.assertIn('throw "product file byte mismatch: $($item.name)"', self.text)
        self.assertIn('throw "product file sha256 mismatch: $($item.name)"', self.text)
        self.assertIn("(Get-Item -LiteralPath $path).Length -ne $item.bytes", self.text)
        self.assertIn("Get-FileHash -Algorithm SHA256 -LiteralPath $path", self.text)
        self.assertIn("$actualHash -ne $item.sha256", self.text)
        self.assertIn("$hash -notmatch '^[0-9a-f]{64}$'", self.text)

    def test_extra_file_and_listing_bounds(self):
        self.assertIn("Get-ChildItem -LiteralPath $stage -File", self.text)
        self.assertIn('throw "unlisted file in product stage: $($file.Name)"', self.text)
        self.assertIn("if ($staged.Count -gt 256)", self.text)
        self.assertIn("if ($entries.Count -eq 0 -or $entries.Count -gt 256)", self.text)

    def test_traversal_and_separator_rejection(self):
        self.assertIn('$name -ne [IO.Path]::GetFileName($name)', self.text)
        self.assertIn('$name.Contains("/")', self.text)
        self.assertIn('$name.Contains("\\")', self.text)
        self.assertIn('$name -eq ".."', self.text)
        self.assertIn('throw "unsafe product inventory name: $name"', self.text)

    def test_zip_output_outside_stage(self):
        self.assertIn("$zipPath.StartsWith($stagePrefix", self.text)
        self.assertIn("$resultPath.StartsWith($stagePrefix", self.text)
        self.assertIn('throw "product package output must be outside the stage"', self.text)
        self.assertIn('Compress-Archive -LiteralPath $sources -DestinationPath $zipPath', self.text)
        self.assertIn('Set-Content -LiteralPath $resultPath', self.text)
        self.assertIn("zip_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $zipPath)", self.text)
        self.assertIn("entries = @($ordered)", self.text)

    def test_no_build_acquisition_or_publication_tokens(self):
        for token in FORBIDDEN:
            with self.subTest(token=token):
                self.assertNotIn(token, self.lower)


if __name__ == "__main__":
    unittest.main(verbosity=2)
