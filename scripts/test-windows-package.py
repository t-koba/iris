#!/usr/bin/env python3
"""Offline contracts for the exact-SHA Windows release package migration."""

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / "scripts/package-windows.ps1"
ISS = ROOT / "scripts/windows-installer.iss"
GENERATOR = ROOT / "scripts/windows-installer-files.py"

_spec = importlib.util.spec_from_file_location("windows_installer_files", GENERATOR)
generator = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(generator)

REQUIRED = (
    '$ErrorActionPreference = "Stop"',
    'throw "missing IRIS_PRODUCT_STAGE"',
    'throw "missing IRIS_PRODUCT_INVENTORY"',
    'throw "missing IRIS_PRODUCT_ZIP"',
    'throw "missing IRIS_RELEASE_OUTPUT_DIR"',
    "cargo metadata --no-deps --format-version 1 --locked",
    "if ($entries.Count -eq 0 -or $entries.Count -gt 256)",
    'throw "invalid product inventory name"',
    '$name.Contains("/")',
    '$name.Contains("\\")',
    '$name -eq ".."',
    'throw "unsafe product inventory name: $name"',
    "$name -match '[\\x00-\\x1f\\x7f\"\\x27;]'",
    "$name -notmatch '^[\\x20-\\x7e]+$'",
    "$hash -notmatch '^[0-9a-f]{64}$'",
    "if ([string]$item.sha256 -cne $hash)",
    "$bytes -le 0 -or $bytes -gt $maxFileBytes",
    'if ($expected.ContainsKey($key)) { throw "duplicate product inventory name: $name" }',
    "Get-ChildItem -LiteralPath $stage -Force",
    "if ($file.PSIsContainer -or ($file.Attributes -band [IO.FileAttributes]::ReparsePoint))",
    'throw "unsafe entry in product stage: $($file.Name)"',
    'if (!$expected.ContainsKey($file.Name.ToLowerInvariant())) { throw "unlisted file in product stage: $($file.Name)" }',
    '@("Iris.exe", "pdfium.dll", "FFMPEG-NOTICE.md")',
    'throw "required product file missing from inventory: $required"',
    'if (!(Test-Path -LiteralPath $path -PathType Leaf)) { throw "product inventory file missing from stage: $($item.name)" }',
    'if ($source.Length -ne $item.bytes) { throw "product file byte mismatch: $($item.name)" }',
    "(Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()",
    'if ($actualHash -ne $item.sha256) { throw "product file sha256 mismatch: $($item.name)" }',
    "if (Test-Path -LiteralPath $distDir) { Remove-Item -LiteralPath $distDir -Recurse -Force }",
    "New-Item -ItemType Directory -Path $distDir",
    "Copy-Item -LiteralPath (Join-Path $stage $name) -Destination (Join-Path $distDir $name)",
    "$env:IRIS_PRODUCT_STAGE = $stage",
    "$env:IRIS_PRODUCT_INVENTORY = $inventoryPath",
    "$env:IRIS_PRODUCT_ZIP = $zipPath",
    "pwsh scripts/windows-product-package.ps1",
    "if (@($productResult.entries).Count -ne $entries.Count)",
    "windows-installer-files.py",
    '"/DInstallerFiles=$includePath"',
    '"/O$outputDir"',
    "if (!(Test-Path -LiteralPath $installerPath -PathType Leaf))",
    'if (![string]::IsNullOrWhiteSpace($env:IRIS_WINDOWS_SIGN_PFX)) {',
    'windows-signing.py") sign `',
    '--file $installerPath `',
    'throw "Windows installer signing failed"',
    'status = "pass"',
    "zip_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $zipPath)",
    "installer_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $installerPath)",
    "installer_bytes = (Get-Item -LiteralPath $installerPath).Length",
    "installer_source_count = $sourceLines.Count",
    "Set-Content -LiteralPath $resultPath",
    "-LiteralPath",
)

FORBIDDEN = (
    "cargo build", "gh release", "invoke-webrequest", "curl", "wget",
    "http://", "https://",
)


def contract_holds(text):
    return all(marker in text for marker in REQUIRED)


def entry(name, size=128, digest=None, **extra):
    record = {
        "name": name,
        "bytes": size,
        "sha256": digest or "a" * 64,
    }
    record.update(extra)
    return record


def write_inventory(directory, files):
    path = directory / "inventory.json"
    path.write_text(json.dumps({"files": files}), encoding="utf-8")
    return path


class PackageScriptContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = PACKAGE.read_text(encoding="utf-8")
        cls.lower = cls.text.lower()

    def test_script_validates_under_offline_contract(self):
        self.assertTrue(contract_holds(self.text))

    def test_every_marker_is_fail_closed(self):
        # Removing any single guard must invalidate the contract.
        for marker in REQUIRED:
            with self.subTest(marker=marker):
                self.assertIn(marker, self.text)
                self.assertFalse(contract_holds(self.text.replace(marker, "<removed>")))

    def test_input_guards_are_distinct(self):
        for name in ("IRIS_PRODUCT_STAGE", "IRIS_PRODUCT_INVENTORY",
                     "IRIS_PRODUCT_ZIP", "IRIS_RELEASE_OUTPUT_DIR"):
            with self.subTest(name=name):
                self.assertIn(f'$env:{name}', self.text)
                self.assertIn(f'throw "missing {name}"', self.text)

    def test_no_build_acquisition_or_publication_tokens(self):
        for token in FORBIDDEN:
            with self.subTest(token=token):
                self.assertNotIn(token, self.lower)

    def test_version_read_is_metadata_only(self):
        self.assertIn("cargo metadata --no-deps --format-version 1 --locked", self.text)
        for line in self.text.splitlines():
            if "cargo" in line.lower():
                self.assertTrue(
                    "cargo metadata" in line.lower() or "cargo resolves" in line.lower(),
                    line)

    def test_signing_runs_after_iscc_and_before_hash_binding(self):
        iscc = self.text.index('throw "Inno Setup did not produce $installerPath"')
        sign = self.text.index('if (![string]::IsNullOrWhiteSpace($env:IRIS_WINDOWS_SIGN_PFX)) {')
        result = self.text.index('$result = @{')
        self.assertLess(iscc, sign)
        self.assertLess(sign, result)

    def test_signing_reads_secrets_from_the_environment(self):
        # Only the guard is named in the packager; the Python helper owns the
        # password and timestamp inputs, so no secret reaches a script line.
        for token in ("IRIS_WINDOWS_SIGN_PFX_PASSWORD",
                      "IRIS_WINDOWS_SIGN_TIMESTAMP_URL", "synthetic-passphrase"):
            self.assertNotIn(token, self.text)

    def test_rehearsal_is_reused_in_process(self):
        bindings = ("$env:IRIS_PRODUCT_STAGE = $stage",
                    "$env:IRIS_PRODUCT_INVENTORY = $inventoryPath",
                    "$env:IRIS_PRODUCT_ZIP = $zipPath")
        call = self.text.index("pwsh scripts/windows-product-package.ps1")
        for binding in bindings:
            self.assertLess(self.text.index(binding), call)


class InstallerScriptContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = ISS.read_text(encoding="utf-8")

    def test_installer_files_define_is_required(self):
        self.assertIn("#ifndef InstallerFiles", self.text)
        self.assertIn("#error InstallerFiles is required", self.text)

    def test_include_is_inside_files_section(self):
        files = self.text.index("[Files]")
        icons = self.text.index("[Icons]")
        include = self.text.index("#include InstallerFiles")
        self.assertLess(files, include)
        self.assertLess(include, icons)

    def test_include_uses_bare_variable_not_literal_placeholder(self):
        # {#...} substitution applies to script lines, not preprocessor
        # directive arguments; a quoted placeholder is opened as a filename.
        self.assertNotIn("{#InstallerFiles}", self.text)

    def test_no_hardcoded_iris_exe_source(self):
        self.assertNotIn('Source: "{#SourceDir}\\Iris.exe"', self.text)
        self.assertIsNone(
            next((line for line in self.text.splitlines()
                  if line.lstrip().startswith("Source:") and "Iris.exe" in line), None))

    def test_identity_and_icons_unchanged(self):
        self.assertIn("AppId={{91ED532A-29D6-4EB6-9692-9EF3994B1DA3}", self.text)
        self.assertIn('Name: "{userprograms}\\Iris"; Filename: "{app}\\Iris.exe"', self.text)
        self.assertIn('Name: "{userdesktop}\\Iris"; Filename: "{app}\\Iris.exe"; Tasks: desktopicon', self.text)


class InstallerFilesContracts(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def test_valid_inventory_generates_exact_lines_in_order(self):
        inventory = write_inventory(self.root, [
            entry("Iris.exe"), entry("pdfium.dll", 4096, "b" * 64),
            entry("FFMPEG-NOTICE.md", 12, "c" * 64),
        ])
        self.assertEqual(generator.installer_files(inventory), [
            'Source: "{#SourceDir}\\Iris.exe"; DestDir: "{app}"; Flags: ignoreversion',
            'Source: "{#SourceDir}\\pdfium.dll"; DestDir: "{app}"; Flags: ignoreversion',
            'Source: "{#SourceDir}\\FFMPEG-NOTICE.md"; DestDir: "{app}"; Flags: ignoreversion',
        ])

    def test_source_bundle_entries_flow_into_installer_source_list(self):
        bundles = ("ffmpeg-8.1.3.tar.xz", "ffmpeg-legal-notices.tar.gz",
                   "ffmpeg-build-materials.tar.gz", "pdfium-notices.tar.gz",
                   "ffmpeg-source-bundle.json")
        inventory = write_inventory(self.root, [entry("Iris.exe")]
                                    + [entry(name) for name in bundles])
        lines = generator.installer_files(inventory)
        self.assertEqual(len(lines), len(bundles) + 1)
        for name, line in zip(bundles, lines[1:]):
            self.assertEqual(line, 'Source: "{#SourceDir}\\' + name
                             + '"; DestDir: "{app}"; Flags: ignoreversion')

    def test_unsafe_names_and_bounds_are_rejected(self):
        cases = {
            "traversal": entry("../Iris.exe"),
            "separator": entry("sub/Iris.exe"),
            "backslash": entry("sub\\Iris.exe"),
            "dot": entry("."),
            "dotdot": entry(".."),
            "duplicate": entry("IRIS.EXE"),
            "oversized_bytes": entry("Iris.exe", generator.MAX_FILE_BYTES + 1),
            "zero_bytes": entry("Iris.exe", 0),
            "bad_hash": entry("Iris.exe", digest="deadbeef"),
            "uppercase_hash": entry("Iris.exe", digest="A" * 64),
            "quote": entry('Iris".exe'),
            "single_quote": entry("Iris'.exe"),
            "newline": entry("Iris\n.exe"),
            "control": entry("Iris\x01.exe"),
            "non_ascii": entry("Irisé.exe"),
            "semicolon": entry("Iris;exe"),
            "long_name": entry("a" * (generator.MAX_NAME_LENGTH + 1)),
        }
        for label, bad in cases.items():
            with self.subTest(case=label), self.assertRaises(ValueError):
                generator.installer_files(write_inventory(
                    self.root, [entry("Iris.exe"), bad]))

    def test_entry_count_bounds(self):
        for files in ([], [entry(f"file{i}.dll") for i in range(generator.MAX_ENTRIES + 1)]):
            with self.subTest(count=len(files)), self.assertRaises(ValueError):
                generator.installer_files(write_inventory(self.root, files))

    def test_cli_writes_exact_lines_with_newline(self):
        inventory = write_inventory(self.root, [entry("Iris.exe")])
        output = self.root / "files.inc"
        self.assertEqual(generator.main([str(inventory), str(output)]), 0)
        self.assertEqual(output.read_text(encoding="utf-8"),
                         'Source: "{#SourceDir}\\Iris.exe"; DestDir: "{app}"; Flags: ignoreversion\n')

    def test_cli_refuses_output_inside_stage(self):
        stage = self.root / "stage"
        stage.mkdir()
        inventory = write_inventory(self.root, [entry("Iris.exe")])
        output = stage / "files.inc"
        with patch.dict(os.environ, {"IRIS_PRODUCT_STAGE": str(stage)}):
            self.assertEqual(generator.main([str(inventory), str(output)]), 1)
        self.assertFalse(output.exists())

    def test_cli_refuses_missing_parent(self):
        inventory = write_inventory(self.root, [entry("Iris.exe")])
        output = self.root / "missing" / "files.inc"
        self.assertEqual(generator.main([str(inventory), str(output)]), 1)
        self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
