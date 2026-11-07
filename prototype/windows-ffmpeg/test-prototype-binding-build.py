"""Offline transaction regressions; not a native Windows/bindgen build."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("binding", HERE / "prototype-binding-build.py")
binding = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(binding)
PINS = json.loads((HERE / "pins.json").read_text())
ORIGINAL = (HERE.parents[1] / "Cargo.lock").read_bytes()


def overridden(raw):
    # Exact production identity only; leave dependency edges and other packages alone.
    old = (f'name = "{binding.NAME}"\nversion = "9.0.1"\n'
           f'source = "{binding.REGISTRY}"\nchecksum = "{binding.CHECKSUM}"')
    new = f'name = "{binding.NAME}"\nversion = "9.1.0"\nsource = "{binding.SOURCE}"'
    text = raw.decode().replace('\r\n', '\n')
    if text.count(old) != 1:
        raise AssertionError("production binding identity changed")
    return text.replace(old, new).encode()


class BindingBuildTests(unittest.TestCase):
    def test_pin_and_single_allowed_lock_transition(self):
        self.assertEqual(binding.COMMIT, "5aa9dc312e0ab9b762bae7ca367d17761d0c9447")
        binding.validate_pin(PINS["ffmpeg_sys_next"])
        binding.validate_lock(ORIGINAL, overridden(ORIGINAL))

    def test_pin_drift_rejected_before_cargo(self):
        for key, value in (("commit", "master"), ("version", "9.0.1"),
                           ("repository", "https://example.invalid/fork")):
            with self.subTest(key=key), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                lock = root / "Cargo.lock"
                lock.write_bytes(ORIGINAL)
                evidence = root / "binding.json"
                with patch.object(binding.subprocess, "run") as run:
                    with self.assertRaises(ValueError):
                        binding.build(root, {**PINS["ffmpeg_sys_next"], key: value}, evidence)
                    run.assert_not_called()
                self.assertEqual(lock.read_bytes(), ORIGINAL)
                self.assertTrue(json.loads(evidence.read_text())["lock_restored"])

    def test_lock_drift_fails_closed(self):
        valid = overridden(ORIGINAL)
        cases = [
            valid.replace(binding.COMMIT.encode(), b"0" * 40),
            valid.replace(b'version = "9.1.0"', b'version = "9.2.0"'),
            valid.replace(b' "num_cpus",', b' "unreviewed-dependency",'),
            valid.replace(b'version = 4', b'version = 3', 1),
            valid + b'\n[[package]]\nname = "unexpected"\nversion = "1.0.0"\n',
            valid + b'\n[[package]]\nname = "ffmpeg-sys-next"\nversion = "9.0.1"\n',
            valid.replace(b'name = "ffmpeg-next"\nversion = "9.0.0"',
                          b'name = "ffmpeg-next"\nversion = "9.1.0"'),
        ]
        for changed in cases:
            with self.subTest(changed=changed[-100:]), self.assertRaises(ValueError):
                binding.validate_lock(ORIGINAL, changed)

    def test_restore_bytes_on_success_and_each_failure_path(self):
        for outcome in ("success", "update_failure", "lock_drift", "build_failure",
                        "build_lock_write", "interrupt"):
            with self.subTest(outcome=outcome), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                lock = root / "Cargo.lock"
                # Noncanonical newlines prove restoration is byte-exact, not reserialization.
                original = ORIGINAL.replace(b'\n', b'\r\n')
                lock.write_bytes(original)
                manifest = root / "Cargo.toml"
                manifest.write_bytes(b"# production manifest must not be edited\r\n")
                evidence = root / "target/binding.json"
                calls = []

                def cargo(command, *, cwd, check):
                    self.assertEqual(cwd, root)
                    self.assertTrue(check)
                    calls.append(command)
                    self.assertEqual(command[-4:], binding.CONFIG)
                    if command[1] == "update":
                        lock.write_bytes(overridden(original))
                        if outcome == "update_failure":
                            raise subprocess.CalledProcessError(1, command)
                        if outcome == "lock_drift":
                            lock.write_bytes(lock.read_bytes() + b'\n[patch]\nunused = []\n')
                    else:
                        self.assertEqual(command, ["cargo", "build", "--locked", "--release",
                                                 "-p", "iris-ui-shell", "--bin", "Iris", *binding.CONFIG])
                        if outcome == "build_failure":
                            raise subprocess.CalledProcessError(1, command)
                        if outcome == "interrupt":
                            raise KeyboardInterrupt
                        if outcome == "build_lock_write":
                            lock.write_bytes(b"unexpected build mutation")

                with patch.object(binding.subprocess, "run", side_effect=cargo):
                    if outcome == "success":
                        binding.build(root, PINS["ffmpeg_sys_next"], evidence)
                    else:
                        with self.assertRaises((ValueError, subprocess.CalledProcessError, KeyboardInterrupt)):
                            binding.build(root, PINS["ffmpeg_sys_next"], evidence)
                self.assertEqual(lock.read_bytes(), original)
                self.assertEqual(manifest.read_bytes(), b"# production manifest must not be edited\r\n")
                self.assertEqual(len(calls), 1 if outcome in ("update_failure", "lock_drift") else 2)
                record = json.loads(evidence.read_text())
                self.assertTrue(record["lock_restored"])
                self.assertEqual(record["status"], "build_passed" if outcome == "success" else "failed")
                self.assertEqual(record["original_lock_sha256"], record["restored_lock_sha256"])
                self.assertIn("unsigned", record["note"])
                self.assertLess(evidence.stat().st_size, 2048)
                self.assertEqual(sorted(p.name for p in root.iterdir()), ["Cargo.lock", "Cargo.toml", "target"])

    def test_stage_and_report_use_override_without_production_configuration(self):
        stage = (HERE / "stage-windows-prototype-app.ps1").read_text()
        self.assertIn('& python (Join-Path $scriptDir "prototype-binding-build.py")', stage)
        self.assertIn('if ($LASTEXITCODE -ne 0) { throw "default-feature Iris build failed" }', stage)
        self.assertNotIn("cargo build", stage)
        report = (HERE / "create-prototype-audit-report.py").read_text()
        self.assertIn('"target/prototype-windows-ffmpeg/binding-override.json"', report)
        self.assertIn('Join-Path $docDir "binding-override.json"', stage)
        production = (HERE.parents[1] / "Cargo.toml").read_text()
        self.assertNotIn(binding.REPOSITORY, production)
        self.assertNotIn(binding.COMMIT, production)


if __name__ == "__main__":
    unittest.main()
