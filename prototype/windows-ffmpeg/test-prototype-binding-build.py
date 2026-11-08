"""Offline transaction regressions; not a native Windows/bindgen build."""
import importlib.util
import json
from pathlib import Path
import re
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
    return binding.override_lock(raw)


def cargo_reresolved(raw):
    # Observed in disposable offline Cargo updates, including one with no override:
    # bindgen's >=2,<4 requirement is re-resolved from syn 3 to syn 2. This is
    # a different dependency edge even though Cargo reports no package additions.
    text = binding.lock_text(raw)
    section = re.compile(r'(?ms)^\[\[package\]\]\nname = "bindgen"\n'
                         r'version = "0\.73\.2"\n.*?(?=^\[\[package\]\]|\Z)')
    matches = list(section.finditer(text))
    if len(matches) != 1 or matches[0].group().count(' "syn 3.0.9",') != 1:
        raise AssertionError("expected one bindgen 0.73.2 edge to syn 3.0.9")
    match = matches[0]
    return (text[:match.start()] + match.group().replace(' "syn 3.0.9",', ' "syn 2.0.119",')
            + text[match.end():]).encode("utf-8")


class BindingBuildTests(unittest.TestCase):
    def test_pin_and_single_allowed_lock_transition(self):
        self.assertEqual(binding.COMMIT, "5aa9dc312e0ab9b762bae7ca367d17761d0c9447")
        binding.validate_pin(PINS["ffmpeg_sys_next"])
        binding.validate_lock(ORIGINAL, overridden(ORIGINAL))

    def test_override_and_validation_normalize_double_crlf(self):
        raw = binding.lock_text(ORIGINAL).encode("utf-8").replace(b"\n", b"\r\r\n")
        binding.validate_lock(raw, overridden(raw))

    def test_override_uses_semantic_identity_across_field_layouts(self):
        old = (f'version = "9.0.1"\nsource = "{binding.REGISTRY}"\n'
               f'checksum = "{binding.CHECKSUM}"')
        reordered = (f'source  = "{binding.REGISTRY}"\n'
                    f'checksum = "{binding.CHECKSUM}"\nversion   = "9.0.1"')
        source = binding.lock_text(ORIGINAL)
        changed = source.replace(old, reordered, 1)
        self.assertNotEqual(changed, source)
        binding.validate_lock(ORIGINAL, overridden(changed.encode("utf-8")))

    def test_override_rejects_binding_identity_drift_without_disclosure(self):
        changed = ORIGINAL.replace(b'version = "9.0.1"', b'version = "9.2.0"', 1)
        with self.assertRaisesRegex(ValueError, "production binding identity drifted") as caught:
            binding.override_lock(changed)
        message = str(caught.exception)
        self.assertNotIn("9.2.0", message)
        self.assertNotIn(binding.CHECKSUM, message)
        self.assertNotIn(binding.REPOSITORY, message)

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
        for index, changed in enumerate(cases):
            with self.subTest(case=index), self.assertRaises(ValueError):
                binding.validate_lock(ORIGINAL, changed)

    def test_cargo_reresolution_is_not_lock_metadata_normalization(self):
        valid = overridden(ORIGINAL)
        changed = cargo_reresolved(valid)
        before = binding.parse_lock(valid)
        after = binding.parse_lock(changed)
        # The package inventory and top-level metadata do not reveal this drift.
        self.assertEqual([(p["name"], p["version"], p.get("source")) for p in before["package"]],
                         [(p["name"], p["version"], p.get("source")) for p in after["package"]])
        self.assertEqual({k: v for k, v in before.items() if k != "package"},
                         {k: v for k, v in after.items() if k != "package"})
        with self.assertRaisesRegex(ValueError, "unrelated Cargo.lock drift"):
            binding.validate_lock(ORIGINAL, changed)

    def test_restore_bytes_on_success_and_each_failure_path(self):
        for outcome in ("success", "lock_drift", "dependency_drift", "build_failure",
                        "build_lock_write", "interrupt"):
            with self.subTest(outcome=outcome), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                lock = root / "Cargo.lock"
                # Noncanonical newlines prove restoration is byte-exact, not reserialization.
                original = binding.lock_text(ORIGINAL).encode("utf-8").replace(b"\n", b"\r\n")
                lock.write_bytes(original)
                manifest = root / "Cargo.toml"
                manifest.write_bytes(b"# production manifest must not be edited\r\n")
                evidence = root / "target/binding.json"
                calls = []
                temporary = overridden(original)
                if outcome == "lock_drift":
                    temporary += b'\n[patch]\nunused = []\n'
                elif outcome == "dependency_drift":
                    temporary = cargo_reresolved(temporary)

                def cargo(command, *, cwd, check):
                    self.assertEqual(cwd, root)
                    self.assertTrue(check)
                    calls.append(command)
                    self.assertEqual(command, ["cargo", "build", "--locked", "--release",
                                               "-p", "iris-ui-shell", "--bin", "Iris", *binding.CONFIG])
                    self.assertEqual(lock.read_bytes(), overridden(original))
                    if outcome == "build_failure":
                        raise subprocess.CalledProcessError(1, command)
                    if outcome == "interrupt":
                        raise KeyboardInterrupt
                    if outcome == "build_lock_write":
                        lock.write_bytes(b"unexpected build mutation")

                with patch.object(binding.subprocess, "run", side_effect=cargo), \
                        patch.object(binding, "override_lock", return_value=temporary):
                    if outcome == "success":
                        binding.build(root, PINS["ffmpeg_sys_next"], evidence)
                    else:
                        with self.assertRaises((ValueError, subprocess.CalledProcessError, KeyboardInterrupt)):
                            binding.build(root, PINS["ffmpeg_sys_next"], evidence)
                self.assertEqual(lock.read_bytes(), original)
                self.assertEqual(manifest.read_bytes(), b"# production manifest must not be edited\r\n")
                self.assertEqual(len(calls), 0 if outcome in ("lock_drift", "dependency_drift") else 1)
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
