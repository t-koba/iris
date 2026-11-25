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

PKGUTIL_MODERN = """Package: Iris-macos.pkg
    Status: signed by a developer certificate issued by Apple for distribution
    Notarization: trusted by the Apple notary service
    Signed with a trusted timestamp on: 2026-01-02 03:04:05 +0000
    Certificate Chain:
     1. Developer ID Installer: Example Corp (ABCDE12345)
        Expires: 2030-01-01
        SHA1 Fingerprint: 0000000000000000000000000000000000000000
"""
PKGUTIL_LEGACY = """Status: signed by a developer certificate that Apple notarized
Notarization: trusted by the Apple notary service
Signed with a trusted timestamp
 1. Developer ID Installer: Example Corp (ABCDE12345)
"""
PKGUTIL_LEGACY_EQUIVALENT = """Status: signed by a developer certificate notarized by Apple
Notarization: trusted by the Apple notary service
Signed with a trusted timestamp
 1. Developer ID Installer: Example Corp (ABCDE12345)
"""
STAPLER_OK = "Processing: Iris-macos.pkg\nThe validate action worked!\n"
GATE_MODERN = ("Iris-macos.pkg: accepted source=Notarized Developer ID "
               "origin=Developer ID Installer: Example Corp (ABCDE12345)\n")
GATE_LEGACY = "Iris-macos.pkg: accepted\nsource=Notarized Developer ID\n"


def evidence(pkg, **overrides):
    pkgutil = m.parse_pkgutil(True, PKGUTIL_MODERN)
    stapler = m.parse_stapler(True, STAPLER_OK)
    gatekeeper = m.parse_gatekeeper(True, GATE_MODERN)
    record = dict(schema=1, artifact=pkg.name, bytes=pkg.stat().st_size,
                  sha256=m.sha256_file(pkg), pkgutil_status=pkgutil,
                  stapler_status=stapler, gatekeeper_status=gatekeeper,
                  pkgutil_text=PKGUTIL_MODERN, stapler_text=STAPLER_OK,
                  gatekeeper_text=GATE_MODERN, verified=True, note="synthetic accepted evidence")
    record.update(overrides)
    return record


def invoke_check(pkg, evidence_path):
    output = StringIO()
    with redirect_stdout(output), patch.object(m.subprocess, "run",
                                               side_effect=AssertionError("tool invoked")):
        result = m.main(["check", "--pkg", str(pkg), "--evidence", str(evidence_path)])
    return result, output.getvalue()


class Parsers(unittest.TestCase):
    def test_pkgutil_accepts_modern_and_legacy_transcripts(self):
        for text in (PKGUTIL_MODERN, PKGUTIL_LEGACY, PKGUTIL_LEGACY_EQUIVALENT):
            with self.subTest(text=text):
                self.assertTrue(m.parse_pkgutil(True, text))

    def test_pkgutil_native_failure_vetoes_accepted_transcript(self):
        self.assertTrue(m.parse_pkgutil(True, PKGUTIL_MODERN))
        self.assertFalse(m.parse_pkgutil(False, PKGUTIL_MODERN))

    def test_pkgutil_rejects_missing_notarization_timestamp_installer_and_rejected(self):
        no_notarization = PKGUTIL_MODERN.replace(
            "    Notarization: trusted by the Apple notary service\n", "")
        no_timestamp = "\n".join(line for line in PKGUTIL_MODERN.splitlines()
                                  if "trusted timestamp" not in line) + "\n"
        application_chain = PKGUTIL_MODERN.replace(
            "Developer ID Installer:", "Developer ID Application:")
        rejected_notarization = PKGUTIL_MODERN.replace(
            "Notarization: trusted by the Apple notary service",
            "Notarization: rejected by Apple")
        for name, text in (
            ("unsigned", "Status: unsigned\n"),
            ("no signature", "Status: no signature\n"),
            ("untrusted certificate", "Status: signed by untrusted certificate\n"),
            ("expired certificate", "Status: signed by a certificate that has since expired\n"),
            ("unnotarized status", "Status: signed by a developer certificate "
             "without notarization\n 1. Developer ID Installer: Example Corp "
             "(ABCDE12345)\n"),
            ("missing notarization line", no_notarization),
            ("notarization without status", PKGUTIL_MODERN.replace(
                "Status: signed by a developer certificate issued by Apple "
                "for distribution\n", "")),
            ("missing trusted timestamp", no_timestamp),
            ("missing Developer ID Installer", application_chain),
            ("rejected notarization", rejected_notarization),
            ("legacy status without markers", "Status: signed by a developer "
             "certificate that Apple notarized\n"),
            ("empty", ""),
        ):
            with self.subTest(case=name):
                self.assertFalse(m.parse_pkgutil(True, text))

    def test_gatekeeper_accepts_modern_verdict_with_extra_fields(self):
        for text in (GATE_MODERN, GATE_LEGACY, "accepted\n"):
            with self.subTest(text=text):
                self.assertTrue(m.parse_gatekeeper(True, text))

    def test_gatekeeper_rejects_rejected_unknown_and_empty_output(self):
        for success, text in (
            (False, GATE_MODERN),
            (True, "Iris-macos.pkg: rejected source=No usable signature\n"),
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
            outputs = ((True, PKGUTIL_MODERN, False), (True, STAPLER_OK, False),
                       (True, GATE_MODERN, False))
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
                       (True, GATE_MODERN, False))
            with patch.object(m, "PLATFORM", "darwin"), patch.object(
                    m, "run_tool", side_effect=outputs):
                self.assertEqual(m.main(["verify", "--pkg", str(pkg),
                                         "--output", str(output)]), 1)
            record = json.loads(output.read_text())
            self.assertFalse(record["pkgutil_status"])
            self.assertFalse(record["verified"])

    def test_synthetic_verify_native_pkgutil_failure_blocks_offline_check(self):
        with tempfile.TemporaryDirectory() as tmp:
            pkg = Path(tmp) / "Iris-macos.pkg"
            pkg.write_bytes(b"synthetic installer bytes")
            output = Path(tmp) / "trust.json"
            outputs = ((False, PKGUTIL_MODERN, False), (True, STAPLER_OK, False),
                       (True, GATE_MODERN, False))
            with patch.object(m, "PLATFORM", "darwin"), patch.object(
                    m, "run_tool", side_effect=outputs):
                self.assertEqual(m.main(["verify", "--pkg", str(pkg),
                                         "--output", str(output)]), 1)
            record = json.loads(output.read_text())
            self.assertIs(record["pkgutil_status"], False)
            self.assertIs(record["stapler_status"], True)
            self.assertIs(record["gatekeeper_status"], True)
            self.assertIs(record["verified"], False)
            result, message = invoke_check(pkg, output)
            self.assertEqual(result, 1)
            self.assertIn("installer trust BLOCKED:", message)

    def test_synthetic_verify_blocks_installer_without_trusted_timestamp(self):
        with tempfile.TemporaryDirectory() as tmp:
            pkg = Path(tmp) / "Iris-macos.pkg"
            pkg.write_bytes(b"synthetic installer bytes")
            output = Path(tmp) / "trust.json"
            no_timestamp = "\n".join(line for line in PKGUTIL_MODERN.splitlines()
                                      if "trusted timestamp" not in line) + "\n"
            outputs = ((True, no_timestamp, False), (True, STAPLER_OK, False),
                       (True, GATE_MODERN, False))
            with patch.object(m, "PLATFORM", "darwin"), patch.object(
                    m, "run_tool", side_effect=outputs):
                self.assertEqual(m.main(["verify", "--pkg", str(pkg),
                                         "--output", str(output)]), 1)
            record = json.loads(output.read_text())
            self.assertFalse(record["pkgutil_status"])
            self.assertFalse(record["verified"])
            self.assertEqual(invoke_check(pkg, output)[0], 1)
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
