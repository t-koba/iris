#!/usr/bin/env python3
"""Offline contracts for tag-only Windows Authenticode signing (synthetic only)."""

import argparse
import contextlib
import hashlib
import importlib.util
import io
from pathlib import Path
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "windows_signing", ROOT / "scripts/windows-signing.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

# Synthetic, non-functional placeholders: no real credential is ever used here.
PFX_PASSWORD = "synthetic-pfx-passphrase-7f3a"
TIMESTAMP_URL = "https://timestamp.example.invalid/rfc3161"
PFX_BYTES = b"synthetic-pfx-payload-not-a-certificate"
TOOL = "C:/synthetic/sdk/bin/x64/signtool.exe"
INSTALLER_NAME = "Iris-windows-setup.exe"
SECRETS = (PFX_PASSWORD,)

GOOD_OUTPUT = """\
Verifying: Iris-windows-setup.exe
Signature Index: 0 (Primary Signature)
Hash of file (sha256): 0123456789ABCDEF
Signing Certificate Chain:
    Issued to: Synthetic Iris
The signature is timestamped: 2026-11-28 00:00:00
Timestamp Verified by:
    Issued to: Synthetic Timestamp Authority
Successfully verified: Iris-windows-setup.exe
Number of files successfully Verified: 1
Number of files failed: 0
"""


def args(**changes):
    values = dict(command="plan", file=INSTALLER_NAME, pfx="synthetic.pfx",
                  password=PFX_PASSWORD, timestamp=TIMESTAMP_URL, signtool=None,
                  output=None)
    values.update(changes)
    return argparse.Namespace(**values)


class RecordingRunner:
    """Impersonates signtool and icacls; records every argv without executing it."""

    def __init__(self, sign_code=0, sign_text="", verify_text=GOOD_OUTPUT,
                 verify_code=0, fail_at=None, acl_code=0):
        self.calls = []
        self.sign_code = sign_code
        self.sign_text = sign_text
        self.verify_text = verify_text
        self.verify_code = verify_code
        self.fail_at = fail_at
        self.acl_code = acl_code
        self.pfx_paths = []
        self.pfx_modes = []
        self.pfx_bytes = []

    def __call__(self, argv):
        self.calls.append(tuple(argv))
        if self.fail_at == len(self.calls):
            raise RuntimeError(f"synthetic tool error {PFX_PASSWORD}")
        if argv[1] == "sign":
            path = Path(argv[argv.index("/f") + 1])
            self.pfx_paths.append(path)
            self.pfx_modes.append(stat.S_IMODE(path.stat().st_mode))
            self.pfx_bytes.append(path.read_bytes())
            return subprocess.CompletedProcess(argv, self.sign_code, self.sign_text, "")
        if argv[0] == "icacls":
            return subprocess.CompletedProcess(argv, self.acl_code, "", "")
        if "/v" not in argv:
            # Real SignTool emits the fields parse_verify needs only with /v.
            return subprocess.CompletedProcess(
                argv, self.verify_code, f"Successfully verified: {INSTALLER_NAME}\n", "")
        return subprocess.CompletedProcess(argv, self.verify_code, self.verify_text, "")


class PlanContracts(unittest.TestCase):
    def setUp(self):
        self.env = {m.PFX_ENV: "synthetic.pfx", m.PASSWORD_ENV: PFX_PASSWORD,
                    m.TIMESTAMP_ENV: TIMESTAMP_URL, m.FILE_ENV: INSTALLER_NAME}

    def test_exact_argv_and_explicit_env_provenance(self):
        record = m.build_plan(TOOL, "synthetic.pfx", PFX_PASSWORD, TIMESTAMP_URL,
                              INSTALLER_NAME, {"pfx": "env:" + m.PFX_ENV,
                                               "password": "env:" + m.PASSWORD_ENV,
                                               "timestamp": "env:" + m.TIMESTAMP_ENV,
                                               "file": "env:" + m.FILE_ENV})
        self.assertEqual(record["argv"], [
            TOOL, "sign", "/fd", "sha256", "/tr", TIMESTAMP_URL, "/td", "sha256",
            "/f", "synthetic.pfx", "/p", PFX_PASSWORD, INSTALLER_NAME])
        self.assertEqual(record["provenance"], {
            "pfx": "env:IRIS_WINDOWS_SIGN_PFX",
            "password": "env:IRIS_WINDOWS_SIGN_PFX_PASSWORD",
            "timestamp": "env:IRIS_WINDOWS_SIGN_TIMESTAMP_URL",
            "file": "env:IRIS_WINDOWS_SIGN_FILE"})
        self.assertEqual(record, m.build_plan(TOOL, "synthetic.pfx", PFX_PASSWORD,
                                              TIMESTAMP_URL, INSTALLER_NAME,
                                              record["provenance"]))

    def test_arg_overrides_env_and_records_provenance(self):
        pfx, password, timestamp, target, provenance = m.plan_inputs(
            args(pfx="arg.pfx", password=PFX_PASSWORD, timestamp=TIMESTAMP_URL,
                 file=INSTALLER_NAME), self.env)
        self.assertEqual((pfx, password, timestamp, target),
                         ("arg.pfx", PFX_PASSWORD, TIMESTAMP_URL, INSTALLER_NAME))
        self.assertEqual(provenance["pfx"], "arg:--pfx")
        self.assertEqual(provenance["password"], "arg:--password")
        self.assertEqual(provenance["timestamp"], "arg:--timestamp")
        self.assertEqual(provenance["file"], "arg:file")

    def test_cli_plan_is_deterministic_and_secret_bearing(self):
        with patch.dict(os.environ, self.env, clear=True), \
                contextlib.redirect_stdout(io.StringIO()) as stdout, \
                contextlib.redirect_stderr(io.StringIO()) as stderr:
            self.assertEqual(m.main(["plan"]), 0)
        record = __import__("json").loads(stdout.getvalue())
        self.assertEqual(record["argv"], m.build_plan(
            "signtool", "synthetic.pfx", PFX_PASSWORD, TIMESTAMP_URL,
            INSTALLER_NAME, record["provenance"])["argv"])
        self.assertIn(PFX_PASSWORD, record["argv"])  # the plan *is* secret-bearing
        self.assertEqual(stderr.getvalue(), "")

    def test_missing_invalid_inputs_fail_closed_without_values(self):
        for missing in (m.PFX_ENV, m.PASSWORD_ENV, m.TIMESTAMP_ENV, m.FILE_ENV):
            with self.subTest(missing=missing), patch.dict(os.environ, self.env, clear=True):
                env = dict(self.env)
                del env[missing]
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr):
                    empty = args(pfx=None, password=None, timestamp=None, file=None)
                    with self.assertRaises(m.SigningError) as caught:
                        m.plan_inputs(empty, env)
                self.assertIn(missing, str(caught.exception))
                self.assertNotIn(PFX_PASSWORD, str(caught.exception))
                self.assertEqual(stderr.getvalue(), "")
        for bad in ("http://timestamp.example.invalid", TIMESTAMP_URL + " bad",
                    "https://", "https://timestamp.example.invalid/\n"):
            with self.subTest(bad=bad):
                env = dict(self.env, **{m.TIMESTAMP_ENV: bad})
                with self.assertRaises(m.SigningError):
                    m.plan_inputs(args(timestamp=None), env)


class VerifyParsingContracts(unittest.TestCase):
    def test_positive_output_with_rfc3161_timestamp(self):
        self.assertTrue(m.parse_verify(True, GOOD_OUTPUT))
        self.assertTrue(m.parse_verify(True, GOOD_OUTPUT.replace(
            "Timestamp Verified by:", "RFc 3161 Timestamp Verified by:")))

    def test_verbose_argv_and_nonverbose_transcript_fails_closed(self):
        self.assertEqual(m.verification_argv(TOOL, INSTALLER_NAME),
                         [TOOL, "verify", "/pa", "/v", INSTALLER_NAME])
        runner = RecordingRunner()
        minimal = runner([TOOL, "verify", "/pa", INSTALLER_NAME])
        self.assertFalse(m.parse_verify(True, minimal.stdout))
        self.assertFalse(m.parse_verify(
            True, "Successfully verified: Iris-windows-setup.exe\n"))

    def test_missing_timestamp_fails(self):
        self.assertFalse(m.parse_verify(True, GOOD_OUTPUT.replace(
            "The signature is timestamped: 2026-11-28 00:00:00\n", "")))
        self.assertFalse(m.parse_verify(True, GOOD_OUTPUT.replace(
            "Timestamp Verified by:", "Timestamp not verified:")))

    def test_errors_and_bad_counts_fail(self):
        self.assertFalse(m.parse_verify(False, GOOD_OUTPUT))
        self.assertFalse(m.parse_verify(True, "Error: SignerSign() failed\n" + GOOD_OUTPUT))
        self.assertFalse(m.parse_verify(True, GOOD_OUTPUT.replace(
            "Number of files failed: 0", "Number of files failed: 1")))
        self.assertFalse(m.parse_verify(True, GOOD_OUTPUT.replace(
            "Number of files successfully Verified: 1",
            "Number of files successfully Verified: 0")))
        self.assertFalse(m.parse_verify(True, GOOD_OUTPUT.replace(
            "Successfully verified: Iris-windows-setup.exe", "")))
        self.assertFalse(m.parse_verify(True, GOOD_OUTPUT, truncated=True))
        self.assertFalse(m.parse_verify(True, GOOD_OUTPUT + "\n" * m.MAX_TEXT_CHARS))

    def test_run_tool_failure_never_returns_exception_text(self):
        def explode(argv):
            raise subprocess.CalledProcessError(1, [TOOL, PFX_PASSWORD], output=PFX_PASSWORD)
        code, text, truncated = m.run_tool(["x"], explode, SECRETS)
        self.assertEqual((code, truncated), (1, True))
        self.assertNotIn(PFX_PASSWORD, text)
        self.assertIn("CalledProcessError", text)

    def test_run_tool_scrubs_secret_variants(self):
        result = subprocess.CompletedProcess([TOOL], 0,
                                             f"using {PFX_PASSWORD} and C:\\synthetic\\pfx", "")
        _, text, truncated = m.run_tool(["x"], lambda argv: result,
                                        (PFX_PASSWORD, "C:/synthetic/pfx"))
        self.assertFalse(truncated)
        self.assertNotIn(PFX_PASSWORD, text)
        self.assertNotIn("C:\\synthetic\\pfx", text)
        self.assertIn("<redacted>", text)


class SignContracts(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.pfx = self.root / "synthetic.pfx"
        self.pfx.write_bytes(PFX_BYTES)
        self.installer = self.root / INSTALLER_NAME
        self.installer.write_bytes(b"synthetic-signed-installer-bytes")
        self.evidence = self.root / "evidence" / "trust.json"
        self.env = {m.PFX_ENV: str(self.pfx), m.PASSWORD_ENV: PFX_PASSWORD,
                    m.TIMESTAMP_ENV: TIMESTAMP_URL, m.FILE_ENV: str(self.installer),
                    m.SIGNTOOL_ENV: TOOL}

    def call(self, runner, platform="posix", **changes):
        namespace = args(command="sign", file=str(self.installer), pfx=str(self.pfx),
                         password=PFX_PASSWORD, timestamp=TIMESTAMP_URL,
                         signtool=TOOL, output=str(self.evidence), **changes)
        return m.do_sign(namespace, self.env, runner, platform)

    def test_sign_then_verify_and_scrubbed_bound_evidence(self):
        noisy = f"signed with {PFX_PASSWORD} and {self.pfx}"
        runner = RecordingRunner(sign_text=noisy, verify_text=GOOD_OUTPUT + noisy)
        self.assertEqual(self.call(runner), 0)
        self.assertEqual([call[1] for call in runner.calls], ["sign", "verify"])
        sign_argv = runner.calls[0]
        self.assertEqual(sign_argv[sign_argv.index("/tr") + 1], TIMESTAMP_URL)
        self.assertEqual(sign_argv[sign_argv.index("/p") + 1], PFX_PASSWORD)
        self.assertNotEqual(sign_argv[sign_argv.index("/f") + 1], str(self.pfx))
        self.assertEqual(runner.calls[1], tuple(m.verification_argv(TOOL, str(self.installer))))
        self.assertIn("/v", runner.calls[1])
        self.assertFalse(any(path.exists() for path in runner.pfx_paths))
        record = __import__("json").loads(self.evidence.read_text())
        self.assertTrue(record["verified"])
        self.assertEqual(record["sha256"], hashlib.sha256(self.installer.read_bytes()).hexdigest())
        self.assertEqual(record["bytes"], self.installer.stat().st_size)
        self.assertEqual(record["artifact"], INSTALLER_NAME)
        evidence_text = self.evidence.read_text()
        for secret in SECRETS + (str(self.pfx),):
            self.assertNotIn(secret, evidence_text)

    def test_private_pfx_is_0600_and_always_removed(self):
        runner = RecordingRunner()
        self.assertEqual(self.call(runner), 0)
        self.assertEqual(runner.pfx_bytes[0], PFX_BYTES)
        if m.POSIX_PFX_MODE:
            self.assertEqual(runner.pfx_modes[0], 0o600)
        failed = RecordingRunner(verify_code=1, verify_text="not verified")
        with self.assertRaises(m.SigningError):
            self.call(failed)
        self.assertFalse(any(path.exists() for path in failed.pfx_paths))

    def test_windows_acl_is_restricted_before_signing(self):
        runner = RecordingRunner()
        self.assertEqual(self.call(runner, platform="nt"), 0)
        self.assertEqual(runner.calls[0][0], "icacls")
        self.assertIn("/inheritance:r", runner.calls[0])
        self.assertTrue(any("/grant:r" in call for call in runner.calls))
        self.assertEqual(runner.calls[0][1], str(runner.pfx_paths[0]))
        denied = RecordingRunner(acl_code=5)
        with self.assertRaises(m.SigningError) as caught:
            self.call(denied, platform="nt")
        self.assertIn("ACL", str(caught.exception))
        self.assertEqual(len(denied.calls), 1)
        self.assertFalse(any(path.exists() for path in denied.pfx_paths))

    def test_sign_succeeds_without_posix_pfx_mode_capability(self):
        # Simulate a host with no fchmod (as on Windows): the POSIX 0600 mode
        # must not be attempted, and the icacls branch must still confine the
        # copy. Patching the module constant keeps this portable on either OS.
        with patch.object(m, "POSIX_PFX_MODE", False):
            copy = m.private_pfx(str(self.pfx))
            try:
                self.assertEqual(Path(copy).read_bytes(), PFX_BYTES)
            finally:
                Path(copy).unlink()
            runner = RecordingRunner()
            self.assertEqual(self.call(runner, platform="nt"), 0)
            self.assertEqual(runner.calls[0][0], "icacls")
            self.assertEqual(runner.pfx_bytes[0], PFX_BYTES)
            self.assertFalse(any(path.exists() for path in runner.pfx_paths))

    def test_tool_errors_and_unverified_timestamps_fail_closed(self):
        cases = (
            RecordingRunner(sign_code=1, sign_text="sign failed"),
            RecordingRunner(verify_code=1, verify_text=GOOD_OUTPUT),
            RecordingRunner(verify_text=GOOD_OUTPUT.replace(
                "The signature is timestamped: 2026-11-28 00:00:00\n", "")),
            RecordingRunner(fail_at=1),
        )
        for runner in cases:
            with self.subTest(sign_code=runner.sign_code, verify_code=runner.verify_code,
                              fail_at=runner.fail_at, text=runner.verify_text[:20]):
                with self.assertRaises(m.SigningError) as caught:
                    self.call(runner)
                self.assertNotIn(PFX_PASSWORD, str(caught.exception))
                self.assertFalse(any(path.exists() for path in runner.pfx_paths))

    def test_cli_error_output_is_scrubbed(self):
        stderr = io.StringIO()
        with patch.dict(os.environ, self.env, clear=True), \
                patch.object(m, "do_sign", side_effect=RuntimeError(PFX_PASSWORD)), \
                contextlib.redirect_stderr(stderr):
            self.assertEqual(m.main(["sign", "--output", str(self.evidence)]), 1)
        self.assertNotIn(PFX_PASSWORD, stderr.getvalue())
        self.assertIn("RuntimeError", stderr.getvalue())

    def test_locator_fails_closed_without_a_tool(self):
        with patch.object(m.shutil, "which", return_value=None), \
                patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(m.SigningError) as caught:
                m.locate_signtool(None, {})
        self.assertIn("signtool was not found", str(caught.exception))


class VerifyCommandContracts(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.installer = Path(self.tmp.name) / INSTALLER_NAME
        self.installer.write_bytes(b"synthetic-installer")
        self.output = Path(self.tmp.name) / "trust.json"

    def check(self, runner):
        namespace = args(command="verify", file=str(self.installer), signtool=TOOL,
                         output=str(self.output))
        return m.do_verify(namespace, {m.SIGNTOOL_ENV: TOOL}, runner)

    def test_verify_writes_bound_trust_evidence(self):
        runner = RecordingRunner()
        self.assertEqual(self.check(runner), 0)
        self.assertEqual(runner.calls, [tuple(m.verification_argv(TOOL, str(self.installer)))])
        self.assertIn("/v", runner.calls[0])
        record = __import__("json").loads(self.output.read_text())
        self.assertTrue(record["verified"])
        self.assertEqual(record["sha256"], hashlib.sha256(self.installer.read_bytes()).hexdigest())

    def test_verify_blocks_on_missing_timestamp(self):
        runner = RecordingRunner(verify_text=GOOD_OUTPUT.replace(
            "The signature is timestamped: 2026-11-28 00:00:00\n", ""))
        with self.assertRaises(m.SigningError):
            self.check(runner)
        record = __import__("json").loads(self.output.read_text())
        self.assertFalse(record["verified"])
        self.assertEqual(record["verify_text"], runner.verify_text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
