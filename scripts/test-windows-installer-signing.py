#!/usr/bin/env python3
"""Offline contracts for compiler-time Inno Setup Authenticode signing.

POSIX hosts cannot run ISCC, so the packager and the .iss are checked as text
while the command model uses windows-signing.py's own plan helper. Synthetic
values are never a credential and are never written into the workspace.
"""

import argparse
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / "scripts/package-windows.ps1"
ISS = ROOT / "scripts/windows-installer.iss"
SIGNING = ROOT / "scripts/windows-signing.py"
_spec = importlib.util.spec_from_file_location("windows_signing", SIGNING)
m = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(m)

# Synthetic placeholders: spaces, a quote and a backslash exercise Windows
# command-line quoting. No real credential or certificate is ever used here.
TOOL = "C:/synthetic/Windows Kits/10/bin/x64/signtool.exe"
PFX = "C:/synthetic/iris signing.pfx"
PASSWORD = 'synthetic pass "quoted" \\ 9d31'
TIMESTAMP = "https://timestamp.example.invalid/rfc3161"
PLACEHOLDER = "$f"
SIGNTOOL_NAME = "IrisCompilerSign"
BASE_ARGS = (
    '"/DAppVersion=$appVersion"',
    '"/DFileVersion=$fileVersion"',
    '"/DInstallerFiles=$includePath"',
    '"/DSourceDir=$distDir"',
    '"/O$outputDir"',
)
ISS_ARG = '(Join-Path $scriptDir "windows-installer.iss")'


def signing_env(**changes):
    values = {
        m.PFX_ENV: PFX,
        m.PASSWORD_ENV: PASSWORD,
        m.TIMESTAMP_ENV: TIMESTAMP,
        m.FILE_ENV: PLACEHOLDER,
        m.SIGNTOOL_ENV: TOOL,
    }
    values.update(changes)
    return values


def provenance():
    return {
        "pfx": "env:" + m.PFX_ENV,
        "password": "env:" + m.PASSWORD_ENV,
        "timestamp": "env:" + m.TIMESTAMP_ENV,
        "file": "arg:file",
    }


def expected_plan():
    return m.build_plan(TOOL, PFX, PASSWORD, TIMESTAMP, PLACEHOLDER, provenance())


def plan_args():
    return argparse.Namespace(command="plan", pfx=None, password=None,
                              timestamp=None, file=PLACEHOLDER, signtool=None,
                              output=None)


def ps_quote(value):
    """Python port of Quote-SignToolArgument in the packager."""
    if not value or not re.search(r'[\s"]', value):
        return value
    quoted = '"'
    slashes = 0
    for char in value:
        if char == "\\":
            slashes += 1
            continue
        if char == '"':
            quoted += "\\" * (slashes * 2 + 1) + '"'
        else:
            quoted += "\\" * slashes + char
        slashes = 0
    return quoted + "\\" * (slashes * 2) + '"'


def compiler_args(plan):
    definition = " ".join(ps_quote(str(value)) for value in plan["argv"])
    return [f"/s{SIGNTOOL_NAME}={definition}", "/DSignedUninstaller=yes"] \
        + list(BASE_ARGS) + [ISS_ARG]


class PackagerContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = PACKAGE.read_text(encoding="utf-8")

    def test_quoting_matches_windows_command_line_rules(self):
        # The packager's character loop must agree with CommandLineToArgvW.
        for value in (TOOL, PFX, PASSWORD, "trailing\\", 'a "quoted" \\ tail',
                      "plain", "with space"):
            with self.subTest(value=value):
                self.assertEqual(ps_quote(value), subprocess.list2cmdline([value]))

    def test_gated_compiler_signing_flow(self):
        markers = (
            "function Quote-SignToolArgument",
            "if ($Value.Length -eq 0 -or $Value -notmatch '[\\s\"]') { return $Value }",
            "$quoted += ('\\' * ($slashes * 2 + 1)) + '\"'",
            "return $quoted + ('\\' * ($slashes * 2)) + '\"'",
            "function Resolve-CompilerSignTool",
            "module.locate_signtool(None, os.environ)",
            "$compilerSigning = ![string]::IsNullOrWhiteSpace($env:IRIS_WINDOWS_SIGN_PFX)",
            "if ($pfxPath.Length -gt 4096 -or $pfxPath -match '[\\x00-\\x1f\\x7f]' -or",
            "!(Test-Path -LiteralPath $pfxPath -PathType Leaf)",
            '"windows-signing.py") plan --signtool $signtool --file \'$f\'',
            "$signtool = Resolve-CompilerSignTool",
            "$plan = $planJson | ConvertFrom-Json",
            "$planJson = $null",
            "$plan.provenance.pfx -cnotlike 'env:*'",
            "$plan.provenance.password -cnotlike 'env:*'",
            "$plan.provenance.timestamp -cnotlike 'env:*'",
            "$definition = ($argv | ForEach-Object { Quote-SignToolArgument $_ }) -join ' '",
            '"/sIrisCompilerSign=$definition", "/DSignedUninstaller=yes"',
            "& $isccPath @isccArgs",
        )
        for marker in markers:
            with self.subTest(marker=marker):
                self.assertIn(marker, self.text)
        ordered = [self.text.index(marker) for marker in (
            "$compilerSigning = ![string]::IsNullOrWhiteSpace($env:IRIS_WINDOWS_SIGN_PFX)",
            "$signtool = Resolve-CompilerSignTool",
            '"windows-signing.py") plan --signtool $signtool --file \'$f\'',
            "$plan = $planJson | ConvertFrom-Json",
            '"/sIrisCompilerSign=$definition", "/DSignedUninstaller=yes"',
            "& $isccPath @isccArgs",
            'if (![string]::IsNullOrWhiteSpace($env:IRIS_WINDOWS_SIGN_PFX)) {',
            "$result = @{")]
        self.assertEqual(ordered, sorted(ordered))

    def test_rehearsal_argv_has_no_signtool_definition(self):
        start = self.text.index("$isccArgs = @(")
        gate = self.text.index("$compilerSigning = ", start)
        base = self.text[start:gate]
        for marker in BASE_ARGS + (ISS_ARG,):
            self.assertIn(marker, base)
        for token in ("/s", "SignedUninstaller", "windows-signing.py",
                      "$env:IRIS_WINDOWS_SIGN_PFX"):
            self.assertNotIn(token, base)
        block = self.text[gate:self.text.index("& $isccPath @isccArgs", gate)]
        self.assertIn("/sIrisCompilerSign=$definition", block)
        self.assertIn("/DSignedUninstaller=yes", block)

    def test_no_credential_literal_in_packager(self):
        for secret in (PASSWORD, PFX, TIMESTAMP):
            self.assertNotIn(secret, self.text)

    def test_final_hash_still_follows_all_signing(self):
        signed = self.text.index('throw "Windows installer signing failed"')
        hashed = self.text.index("installer_sha256 = (Get-FileHash")
        self.assertLess(signed, hashed)


class IssContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = ISS.read_text(encoding="utf-8")

    def test_activation_only_exists_behind_compile_time_define(self):
        block = self.text.split("#ifdef SignedUninstaller", 1)[1].split("#endif", 1)[0]
        self.assertIn("SignTool=IrisCompilerSign", block)
        self.assertIn("SignedUninstaller=yes", block)
        outside = self.text.replace(block, "")
        for line in outside.splitlines():
            stripped = line.strip()
            self.assertFalse(stripped.startswith("SignTool="), stripped)
            self.assertFalse(stripped.startswith("SignedUninstaller="), stripped)
        self.assertEqual(self.text.count("#ifdef SignedUninstaller"), 1)

    def test_sign_tool_name_matches_compiler_definition(self):
        self.assertIn("/sIrisCompilerSign=", PACKAGE.read_text(encoding="utf-8"))
        self.assertIn("SignTool=IrisCompilerSign", self.text)
        self.assertIn("#ifdef SignedUninstaller", self.text)


class CommandModelContracts(unittest.TestCase):
    def test_helper_plan_is_the_sign_tool_definition_source(self):
        with patch.dict(os.environ, signing_env(), clear=True), \
                contextlib.redirect_stdout(io.StringIO()) as stdout, \
                contextlib.redirect_stderr(io.StringIO()) as stderr:
            self.assertEqual(m.main(["plan", "--signtool", TOOL, "--file", PLACEHOLDER]), 0)
        self.assertEqual(stderr.getvalue(), "")
        # The plan is secret-bearing: check it in memory only, never persist it.
        record = json.loads(stdout.getvalue())
        expected = expected_plan()
        self.assertTrue(record["argv"] == expected["argv"])
        self.assertTrue(record["provenance"] == expected["provenance"])

    def test_signed_argv_defines_sign_tool_and_signed_uninstaller(self):
        plan = expected_plan()
        with patch.object(m.shutil, "which", return_value=TOOL):
            self.assertEqual(m.locate_signtool(None, signing_env()), TOOL)
        argv = compiler_args(plan)
        self.assertEqual(argv[0], f"/s{SIGNTOOL_NAME}=" + subprocess.list2cmdline(plan["argv"]))
        self.assertTrue(argv[0].endswith(PLACEHOLDER))
        self.assertIn("/DSignedUninstaller=yes", argv)
        self.assertEqual(argv[2:], list(BASE_ARGS) + [ISS_ARG])
        self.assertEqual(plan["argv"][1], "sign")
        self.assertEqual(plan["argv"][-1], PLACEHOLDER)

    def test_unsigned_rehearsal_has_no_signing_switch(self):
        argv = list(BASE_ARGS) + [ISS_ARG]
        self.assertFalse(any(arg.startswith("/s") for arg in argv))
        self.assertFalse(any("SignedUninstaller" in arg for arg in argv))

    def test_blank_pfx_keeps_rehearsal_unsigned(self):
        text = PACKAGE.read_text(encoding="utf-8")
        self.assertIn(
            "$compilerSigning = ![string]::IsNullOrWhiteSpace($env:IRIS_WINDOWS_SIGN_PFX)",
            text)
        for blank in ("", "  \t"):
            with self.subTest(blank=blank.encode("unicode_escape")):
                # The packager skips both compiler and outer signing on blank.
                self.assertFalse(bool(blank) and not blank.isspace())

    def test_malformed_pfx_fails_closed_without_leaking(self):
        text = PACKAGE.read_text(encoding="utf-8")
        self.assertIn("$pfxPath -match '[\\x00-\\x1f\\x7f]'", text)
        self.assertIn("!(Test-Path -LiteralPath $pfxPath -PathType Leaf)", text)
        for bad in (PFX + "\n", PFX + "\x7f"):
            with self.subTest(bad=bad.encode("unicode_escape")):
                with self.assertRaises(m.SigningError) as caught:
                    m.plan_inputs(plan_args(), signing_env(**{m.PFX_ENV: bad}))
                message = str(caught.exception)
                self.assertNotIn(PASSWORD, message)
                self.assertNotIn(PFX, message)

    def test_no_credential_is_written_into_the_workspace(self):
        scripts = ROOT / "scripts"
        before = {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                  for path in scripts.iterdir() if path.is_file()}
        paths_before = {path for path in ROOT.rglob("*")}
        with patch.dict(os.environ, signing_env(), clear=True), \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(m.main(["plan"]), 0)
        compiler_args(expected_plan())
        after = {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                 for path in scripts.iterdir() if path.is_file()}
        self.assertEqual(before, after)
        self.assertEqual(paths_before, {path for path in ROOT.rglob("*")})
        # Temporary data is confined to the OS temp directory, never the repo.
        with tempfile.TemporaryDirectory() as tmp:
            self.assertTrue(Path(tmp).is_dir())


if __name__ == "__main__":
    unittest.main(verbosity=2)
