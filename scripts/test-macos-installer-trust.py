#!/usr/bin/env python3
"""Offline contracts for final macOS installer trust evidence and its release gate."""

from contextlib import redirect_stdout
from io import StringIO
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/macos-installer-trust.py"
spec = importlib.util.spec_from_file_location("macos_installer_trust", SCRIPT)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

PKGUTIL_OK = """Package: dev.tk.iris.pkg
Status: signed by a developer certificate that Apple notarized
Certificate Chain:
 1. Developer ID Installer: Example Corp (ABCDE12345)
    Expires: 2030-01-01
"""
PKGUTIL_EQUIVALENT = """Status: signed by a developer certificate notarized by Apple
 1. Developer ID Installer: Example Corp (ABCDE12345)
"""
STAPLER_OK = "Processing: Iris-macos.pkg\nThe validate action worked!\n"
GATE_OK = "Iris-macos.pkg: accepted\nsource=Notarized Developer ID\n"


def evidence(pkg, **overrides):
    pkgutil = m.parse_pkgutil(PKGUTIL_OK)
    stapler = m.parse_stapler(True, STAPLER_OK)
    gatekeeper = m.parse_gatekeeper(True, GATE_OK)
    record = dict(schema=1, artifact=pkg.name, bytes=pkg.stat().st_size,
                  sha256=m.sha256_file(pkg), pkgutil_status=pkgutil,
                  stapler_status=stapler, gatekeeper_status=gatekeeper,
                  pkgutil_text=PKGUTIL_OK, stapler_text=STAPLER_OK,
                  gatekeeper_text=GATE_OK, verified=True, note="synthetic accepted evidence")
    record.update(overrides)
    return record


def invoke_check(pkg, evidence_path):
    output = StringIO()
    with redirect_stdout(output), patch.object(m.subprocess, "run",
                                               side_effect=AssertionError("tool invoked")):
        result = m.main(["check", "--pkg", str(pkg), "--evidence", str(evidence_path)])
    return result, output.getvalue()


class Parsers(unittest.TestCase):
    def test_pkgutil_requires_notarized_developer_id_installer(self):
        for text in (PKGUTIL_OK, PKGUTIL_EQUIVALENT):
            with self.subTest(text=text):
                self.assertTrue(m.parse_pkgutil(text))
        for text in (
            "Status: unsigned\n",
            "Status: signed by a developer certificate without notarization\n"
            " 1. Developer ID Installer: Example Corp (ABCDE12345)\n",
            "Status: signed by a developer certificate that Apple notarized\n",
            "Status: signed by a developer certificate that Apple notarized\n"
            " 1. Developer ID Application: Example Corp (ABCDE12345)\n",
            "Status: signed by a developer certificate that Apple notarized but not stapled\n",
            "",
        ):
            with self.subTest(text=text):
                self.assertFalse(m.parse_pkgutil(text))

    def test_gatekeeper_requires_explicit_accepted_verdict(self):
        self.assertTrue(m.parse_gatekeeper(True, GATE_OK))
        self.assertTrue(m.parse_gatekeeper(True, "accepted\n"))
        for success, text in (
            (False, GATE_OK), (True, "Iris-macos.pkg: rejected\n"),
            (True, "Iris-macos.pkg: not accepted\nsource=Unnotarized Developer ID\n"),
            (True, "source=Notarized Developer ID\n"), (True, ""),
        ):
            with self.subTest(success=success, text=text):
                self.assertFalse(m.parse_gatekeeper(success, text))

    def test_stapler_requires_success(self):
        self.assertTrue(m.parse_stapler(True, STAPLER_OK))
        self.assertFalse(m.parse_stapler(False, STAPLER_OK))
        self.assertFalse(m.parse_stapler(True, "The validate action failed!\n"))

    def test_clean_removes_paths_certificate_names_and_profiles(self):
        with tempfile.TemporaryDirectory() as tmp:
            with patch.dict(os.environ, {"HOME": tmp, "GITHUB_WORKSPACE": tmp + "/repo",
                                         "RUNNER_TEMP": tmp + "/temp"}):
                text = m.clean(
                    f"Processing: {tmp}/repo/dist/macos/Iris-macos.pkg\n"
                    f"{tmp}/temp/scratch\n"
                    " 1. Developer ID Installer: Private Corp (ABCDE12345)\n"
                    "keychain-profile private-profile\n"
                )
            self.assertNotIn(tmp, text)
            self.assertNotIn("Private Corp", text)
            self.assertNotIn("private-profile", text)
            self.assertNotIn("ABCDE12345", text)
            self.assertIn("Developer ID Installer: <redacted>", text)
            self.assertIn("keychain-profile <redacted>", text)

    def test_tool_output_is_bounded(self):
        completed = subprocess.CompletedProcess([], 0, b"accepted\n" + b"x" * (m.MAX_TOOL_BYTES + 1))
        with patch.object(m.subprocess, "run", return_value=completed):
            success, text, truncated = m.run_tool(["spctl"])
        self.assertTrue(success)
        self.assertTrue(truncated)
        self.assertLessEqual(len(text), m.MAX_TEXT_CHARS)


class Producer(unittest.TestCase):
    def test_verify_fails_closed_off_macos_without_running_tools(self):
        with tempfile.TemporaryDirectory() as tmp:
            pkg = Path(tmp) / "Iris-macos.pkg"
            pkg.write_bytes(b"pkg")
            output = Path(tmp) / "audit" / "trust.json"
            with patch.object(m, "PLATFORM", "linux"), patch.object(
                    m.subprocess, "run", side_effect=AssertionError("tool invoked")):
                self.assertEqual(m.main(["verify", "--pkg", str(pkg), "--output", str(output)]), 1)
            self.assertFalse(output.exists())

    def test_synthetic_verify_writes_scrubbed_evidence_then_offline_check_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            pkg = Path(tmp) / "Iris-macos.pkg"
            pkg.write_bytes(b"synthetic installer bytes")
            output = Path(tmp) / "audit" / "trust.json"
            outputs = ((True, PKGUTIL_OK, False), (True, STAPLER_OK, False),
                       (True, GATE_OK, False))
            with patch.object(m, "PLATFORM", "darwin"), patch.object(
                    m, "run_tool", side_effect=outputs):
                self.assertEqual(m.main(["verify", "--pkg", str(pkg),
                                         "--output", str(output)]), 0)
            raw = output.read_text()
            record = json.loads(raw)
            self.assertEqual(record["schema"], 1)
            self.assertEqual(record["bytes"], pkg.stat().st_size)
            self.assertEqual(record["sha256"], m.sha256_file(pkg))
            self.assertTrue(all(record[key] is True for key in m.STATUS_FIELDS))
            self.assertTrue(record["verified"])
            self.assertNotIn("Example Corp", raw)
            self.assertNotIn(tmp, raw)
            self.assertEqual(invoke_check(pkg, output)[0], 0)
            completed = subprocess.run(
                [sys.executable, str(SCRIPT), "check", "--pkg", str(pkg),
                 "--evidence", str(output)], capture_output=True, text=True, timeout=10)
            self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)
            self.assertIn("installer trust PASS Iris-macos.pkg", completed.stdout)

    def test_synthetic_verify_blocks_unnotarized_installer(self):
        with tempfile.TemporaryDirectory() as tmp:
            pkg = Path(tmp) / "Iris-macos.pkg"
            pkg.write_bytes(b"synthetic installer bytes")
            output = Path(tmp) / "trust.json"
            unsigned = "Status: unsigned\n"
            outputs = ((True, unsigned, False), (True, STAPLER_OK, False),
                       (True, GATE_OK, False))
            with patch.object(m, "PLATFORM", "darwin"), patch.object(
                    m, "run_tool", side_effect=outputs):
                self.assertEqual(m.main(["verify", "--pkg", str(pkg),
                                         "--output", str(output)]), 1)
            record = json.loads(output.read_text())
            self.assertFalse(record["pkgutil_status"])
            self.assertFalse(record["verified"])
            self.assertIn("BLOCKED", invoke_check(pkg, output)[1])


class Consumer(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.pkg = self.root / "Iris-macos.pkg"
        self.pkg.write_bytes(b"exact installer bytes")
        self.evidence_path = self.root / "trust.json"

    def write(self, record):
        m.write_evidence(self.evidence_path, record)
        return self.evidence_path

    def test_check_accepts_exact_bytes(self):
        result, output = invoke_check(self.pkg, self.write(evidence(self.pkg)))
        self.assertEqual(result, 0)
        self.assertIn("installer trust PASS Iris-macos.pkg", output)
        self.assertIn(hashlib.sha256(b"exact installer bytes").hexdigest(), output)

    def test_check_rejects_bad_bindings_and_failed_gates(self):
        cases = {
            "hash mismatch": evidence(self.pkg, sha256="0" * 64),
            "size mismatch": evidence(self.pkg, bytes=self.pkg.stat().st_size + 1),
            "artifact mismatch": evidence(self.pkg, artifact="Other.pkg"),
            "bad schema": dict(evidence(self.pkg), schema=2),
            "missing field": {k: v for k, v in evidence(self.pkg).items() if k != "note"},
            "duplicate key": None,
            "non-accepted gatekeeper": evidence(self.pkg, gatekeeper_status=False),
            "missing notarization": evidence(self.pkg, pkgutil_status=False),
            "not stapled": evidence(self.pkg, stapler_status=False),
            "unverified": evidence(self.pkg, verified=False),
            "truthy status is not enough": evidence(self.pkg, gatekeeper_status="true"),
        }
        for name, record in cases.items():
            with self.subTest(case=name):
                if record is None:
                    self.evidence_path.write_text(
                        json.dumps(evidence(self.pkg))[:-1] + ', "verified": false}\n')
                else:
                    self.write(record)
                result, output = invoke_check(self.pkg, self.evidence_path)
                self.assertEqual(result, 1)
                self.assertIn("installer trust BLOCKED:", output)

    def test_check_rejects_oversized_and_malformed_evidence(self):
        self.evidence_path.write_bytes(b"x" * (m.MAX_EVIDENCE_BYTES + 1))
        self.assertEqual(invoke_check(self.pkg, self.evidence_path)[0], 1)
        self.evidence_path.write_text('{"schema":1,')
        result, output = invoke_check(self.pkg, self.evidence_path)
        self.assertEqual(result, 1)
        self.assertIn("BLOCKED", output)


if __name__ == "__main__":
    unittest.main()
