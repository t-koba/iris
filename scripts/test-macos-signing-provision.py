#!/usr/bin/env python3
"""Offline contracts for tag-only macOS signing credential provisioning."""
import base64
import contextlib
import importlib.util
import io
import os
import re
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "macos_signing_provision", ROOT / "scripts/macos-signing-provision.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

# Synthetic, non-functional placeholders: no real credential is ever used here.
SIGN_IDENTITY = "Developer ID Application: Synthetic Iris (S3CR3TTEAM)"
INSTALLER_IDENTITY = "Developer ID Installer: Synthetic Iris (S3CR3TTEAM)"
APP_PROFILE = "synthetic-app-notary-profile"
INSTALLER_PROFILE = "synthetic-installer-notary-profile"
P12_BYTES = b"synthetic-p12-payload-not-a-certificate"
P12_B64 = base64.b64encode(P12_BYTES).decode("ascii")
BAD_B64 = "!!!synthetic-not-base64!!!"
P12_PASSWORD = "synthetic-p12-passphrase-7f3a"
KEYCHAIN_PASSWORD = "synthetic-keychain-passphrase-9d2b"
APPLE_ID = "synthetic-apple-id@example.invalid"
TEAM_ID = "S3CR3TTEAM"
APP_PASSWORD = "synthetic-app-passphrase-4c8e"
SECRET_VALUES = (SIGN_IDENTITY, INSTALLER_IDENTITY, APP_PROFILE, INSTALLER_PROFILE,
                 P12_B64, P12_BYTES.decode("ascii"), BAD_B64, P12_PASSWORD,
                 KEYCHAIN_PASSWORD, APPLE_ID, TEAM_ID, APP_PASSWORD)


class RecordingRunner:
    """Captures argv and impersonates only the identity-listing tool output."""

    def __init__(self, identity_mode="both", fail_at=None):
        self.calls = []
        self.identity_mode = identity_mode
        self.fail_at = fail_at
        self.p12_path = None
        self.p12_mode = None
        self.p12_bytes = None

    def __call__(self, argv):
        self.calls.append(tuple(argv))
        if self.fail_at == len(self.calls):
            raise RuntimeError(f"synthetic tool failure with {P12_PASSWORD}")
        if tuple(argv[:2]) == ("security", "import"):
            p12 = Path(argv[2])
            self.p12_path = p12
            self.p12_mode = stat.S_IMODE(p12.stat().st_mode)
            self.p12_bytes = p12.read_bytes()
        if tuple(argv[:2]) == ("security", "find-identity"):
            identities = []
            if self.identity_mode in ("both", "app-only"):
                identities.append(SIGN_IDENTITY)
            if self.identity_mode == "both":
                identities.append(INSTALLER_IDENTITY)
            listing = "".join(f'  {i + 1}) SYNTHETIC{i + 1} "{name}"\n'
                              for i, name in enumerate(identities))
            return subprocess.CompletedProcess(argv, 0, listing, "")
        return subprocess.CompletedProcess(argv, 0, "", "")


class WorkflowContextContracts(unittest.TestCase):
    def test_release_workflow_uses_step_scoped_runner_temp(self):
        release = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
        step_line = (
            "          IRIS_MACOS_KEYCHAIN_PATH: ${{ runner.temp }}/iris-signing.keychain-db"
        )
        # GitHub allows `runner` only in steps.env. Cover every expression in
        # the file so a later hoist to top-level or job-level env cannot pass.
        self.assertEqual([line for line in release.splitlines()
                          if re.search(r"\brunner\b", line, re.IGNORECASE)],
                         [step_line, step_line])
        job = release.split("  package-macos:\n", 1)[1].split("\n  package-linux:\n", 1)[0]
        for step_name in ("Provision macOS signing credentials", "Package macOS app"):
            marker = f"      - name: {step_name}\n"
            step = job.split(marker, 1)[1].split("\n      - name: ", 1)[0]
            self.assertIn(step_line + "\n", step)


class ProvisioningContracts(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.keychain = str(Path(tmp.name) / "iris-signing.keychain-db")

    def sample_env(self, **changes):
        env = {
            "IRIS_MACOS_SIGN_IDENTITY": SIGN_IDENTITY,
            "IRIS_MACOS_INSTALLER_IDENTITY": INSTALLER_IDENTITY,
            "IRIS_MACOS_NOTARY_PROFILE": APP_PROFILE,
            "IRIS_MACOS_INSTALLER_NOTARY_PROFILE": INSTALLER_PROFILE,
            "IRIS_MACOS_SIGNING_P12_BASE64": P12_B64,
            "IRIS_MACOS_SIGNING_P12_PASSWORD": P12_PASSWORD,
            "IRIS_MACOS_KEYCHAIN_PASSWORD": KEYCHAIN_PASSWORD,
            "IRIS_MACOS_APPLE_ID": APPLE_ID,
            "IRIS_MACOS_TEAM_ID": TEAM_ID,
            "IRIS_MACOS_APP_PASSWORD": APP_PASSWORD,
            "IRIS_MACOS_KEYCHAIN_PATH": self.keychain,
        }
        env.update(changes)
        return env

    def assert_no_secrets(self, text):
        for value in SECRET_VALUES:
            self.assertNotIn(value, text)

    def test_exact_command_order_and_distinct_notary_profiles(self):
        runner = RecordingRunner()
        m.provision(self.sample_env(), runner)
        p12_path = str(runner.p12_path)
        expected = [
            ("security", "create-keychain", "-p", KEYCHAIN_PASSWORD, self.keychain),
            ("security", "set-keychain-settings", "-lut", "21600", self.keychain),
            ("security", "unlock-keychain", "-p", KEYCHAIN_PASSWORD, self.keychain),
            ("security", "import", p12_path, "-k", self.keychain, "-P", P12_PASSWORD,
             "-T", "/usr/bin/codesign", "-T", "/usr/bin/productsign"),
            ("security", "set-key-partition-list", "-S", "apple-tool:,apple:", "-s",
             "-k", KEYCHAIN_PASSWORD, self.keychain),
            ("security", "list-keychains", "-d", "user", "-s", self.keychain),
            ("xcrun", "notarytool", "store-credentials", APP_PROFILE,
             "--apple-id", APPLE_ID, "--team-id", TEAM_ID, "--password", APP_PASSWORD,
             "--keychain", self.keychain),
            ("xcrun", "notarytool", "store-credentials", INSTALLER_PROFILE,
             "--apple-id", APPLE_ID, "--team-id", TEAM_ID, "--password", APP_PASSWORD,
             "--keychain", self.keychain),
            ("security", "find-identity", "-v", "-p", "codesigning", self.keychain),
            ("security", "find-identity", "-v", "-p", "basic", self.keychain),
        ]
        self.assertEqual(runner.calls, expected)
        stored = [c for c in runner.calls
                  if c[:3] == ("xcrun", "notarytool", "store-credentials")]
        self.assertEqual(len(stored), 2)
        self.assertEqual([c[3] for c in stored], [APP_PROFILE, INSTALLER_PROFILE])
        for call in stored:
            self.assertEqual(call[call.index("--keychain") + 1], self.keychain)
        for call in runner.calls[-2:]:
            self.assertEqual(call[-1], self.keychain)

    def test_temp_p12_is_0600_and_always_removed(self):
        runner = RecordingRunner()
        m.provision(self.sample_env(), runner)
        self.assertEqual(runner.p12_bytes, P12_BYTES)
        self.assertEqual(runner.p12_mode, 0o600)
        self.assertFalse(runner.p12_path.exists())

        failed = RecordingRunner(fail_at=5)
        with self.assertRaises(m.ProvisioningError):
            m.provision(self.sample_env(), failed)
        self.assertEqual(failed.p12_mode, 0o600)
        self.assertFalse(failed.p12_path.exists())

    def test_missing_and_invalid_inputs_fail_closed_without_values(self):
        for name in m.REQUIRED_NAMES:
            with self.subTest(missing=name):
                env = self.sample_env()
                del env[name]
                runner = RecordingRunner()
                with self.assertRaises(m.ProvisioningError) as caught:
                    m.provision(env, runner)
                self.assertIn(name, str(caught.exception))
                self.assertEqual(runner.calls, [])
                self.assert_no_secrets(str(caught.exception))

        invalid = (
            ("IRIS_MACOS_SIGN_IDENTITY", "-", "IRIS_MACOS_SIGN_IDENTITY"),
            ("IRIS_MACOS_INSTALLER_IDENTITY", "-", "IRIS_MACOS_INSTALLER_IDENTITY"),
            ("IRIS_MACOS_NOTARY_PROFILE", "   ", "IRIS_MACOS_NOTARY_PROFILE"),
            ("IRIS_MACOS_INSTALLER_NOTARY_PROFILE", "", "IRIS_MACOS_INSTALLER_NOTARY_PROFILE"),
            ("IRIS_MACOS_SIGNING_P12_BASE64", BAD_B64, "IRIS_MACOS_SIGNING_P12_BASE64"),
            ("IRIS_MACOS_SIGNING_P12_BASE64", P12_B64 + "\n", "IRIS_MACOS_SIGNING_P12_BASE64"),
            ("IRIS_MACOS_SIGNING_P12_MAX_BYTES", "zero", "IRIS_MACOS_SIGNING_P12_MAX_BYTES"),
            ("IRIS_MACOS_SIGNING_P12_MAX_BYTES", "0", "IRIS_MACOS_SIGNING_P12_MAX_BYTES"),
            ("IRIS_MACOS_SIGNING_P12_MAX_BYTES", "1", "IRIS_MACOS_SIGNING_P12_MAX_BYTES"),
        )
        for name, value, named_in_error in invalid:
            with self.subTest(invalid=name, value=value):
                runner = RecordingRunner()
                with self.assertRaises(m.ProvisioningError) as caught:
                    m.provision(self.sample_env(**{name: value}), runner)
                self.assertIn(named_in_error, str(caught.exception))
                self.assertEqual(runner.calls, [])
                self.assert_no_secrets(str(caught.exception))

    def test_shared_notary_profile_is_allowed(self):
        runner = RecordingRunner()
        m.provision(self.sample_env(
            IRIS_MACOS_INSTALLER_NOTARY_PROFILE=APP_PROFILE), runner)
        stored = [c for c in runner.calls
                  if c[:3] == ("xcrun", "notarytool", "store-credentials")]
        self.assertEqual([c[3] for c in stored], [APP_PROFILE, APP_PROFILE])

    def test_identity_mismatch_fails_closed_and_cleans_up(self):
        runner = RecordingRunner(identity_mode="app-only")
        with self.assertRaises(m.ProvisioningError) as caught:
            m.provision(self.sample_env(), runner)
        self.assertIn("IRIS_MACOS_INSTALLER_IDENTITY", str(caught.exception))
        self.assert_no_secrets(str(caught.exception))
        self.assertFalse(runner.p12_path.exists())

    def test_command_failure_and_cli_output_are_redacted(self):
        runner = RecordingRunner(fail_at=5)
        with self.assertRaises(m.ProvisioningError) as caught:
            m.provision(self.sample_env(), runner)
        self.assertIn("security set-key-partition-list", str(caught.exception))
        self.assertIn("RuntimeError", str(caught.exception))
        self.assert_no_secrets(str(caught.exception))

        def failed_run(argv, **kwargs):
            raise subprocess.CalledProcessError(1, ["security", P12_PASSWORD],
                                                output=P12_PASSWORD)

        stderr = io.StringIO()
        with patch.dict(os.environ, self.sample_env()), \
                patch.object(m.subprocess, "run", side_effect=failed_run), \
                contextlib.redirect_stderr(stderr):
            self.assertEqual(m.main([]), 1)
        self.assert_no_secrets(stderr.getvalue())

    def test_production_runner_checks_and_captures_every_command(self):
        seen = []

        def fake_run(argv, **kwargs):
            seen.append((tuple(argv), kwargs))
            listing = f'  1) SYN "{SIGN_IDENTITY}"\n  2) SYN "{INSTALLER_IDENTITY}"\n'
            return subprocess.CompletedProcess(argv, 0, listing, "")

        stderr = io.StringIO()
        with patch.dict(os.environ, self.sample_env()), \
                patch.object(m.subprocess, "run", side_effect=fake_run), \
                contextlib.redirect_stderr(stderr):
            self.assertEqual(m.main([]), 0)
        self.assertEqual(len(seen), 10)
        for _, kwargs in seen:
            self.assertTrue(kwargs["check"])
            self.assertEqual(kwargs["stdout"], subprocess.PIPE)
            self.assertEqual(kwargs["stderr"], subprocess.STDOUT)
        import_call = next(argv for argv, _ in seen if argv[:2] == ("security", "import"))
        self.assertFalse(Path(import_call[2]).exists())
        self.assertEqual(stderr.getvalue(), "")


if __name__ == "__main__":
    sys.exit(0 if unittest.main(exit=False).result.wasSuccessful() else 1)
