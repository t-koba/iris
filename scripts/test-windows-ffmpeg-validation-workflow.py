#!/usr/bin/env python3
"""Offline contracts for the Windows native-validation workflow guard."""

import importlib.util
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]


def load():
    spec = importlib.util.spec_from_file_location(
        "wvwf", ROOT / "scripts/windows-ffmpeg-validation-workflow.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class WorkflowContracts(unittest.TestCase):
    def setUp(self):
        self.mod = load()
        self.text = (ROOT / ".github/workflows/windows-ffmpeg-validation.yml").read_text()

    def test_real_workflow_validates(self):
        checks = self.mod.validate(self.text)
        self.assertEqual(checks["reusable_only"], True)
        self.assertEqual(checks["upload_count"], 1)
        self.assertTrue(checks["evidence_json_only"])

    def test_all_offline_gates_present(self):
        for gate in self.mod.OFFLINE_GATES:
            self.assertIn(gate, self.text)

    def test_contract_cli_emits_shape(self):
        import json, subprocess
        r = subprocess.run(
            [sys.executable, str(ROOT / "scripts/windows-ffmpeg-validation-workflow.py"), "contract"],
            capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        shape = json.loads(r.stdout)
        self.assertEqual(shape["triggers"], ["workflow_call"])


class FailClosedContracts(unittest.TestCase):
    def setUp(self):
        self.mod = load()
        self.text = (ROOT / ".github/workflows/windows-ffmpeg-validation.yml").read_text()

    def test_rejects_dispatch_trigger(self):
        mutated = self.text.replace("on:\n  workflow_call:\n", "on:\n  workflow_dispatch:\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_release_promotion_marker(self):
        mutated = self.text.replace("Build product with stock locked bindings",
                                    "Build product with IRIS_BUILD_VARIANT: minimal")
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_unsigned_source_marker(self):
        mutated = self.text.replace('if ($pin.version -ne "8.1.3")', 'if ($pin.version -ne "9.1.0")', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_missing_offline_gate(self):
        mutated = self.text.replace(
            "        run: python3 scripts/test-windows-ffmpeg-source.py\n", "")
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_publishing_action(self):
        mutated = self.text.replace(
            "      - name: Assemble bounded evidence manifest",
            "      - uses: softprops/action-gh-release@v2\n      - name: Assemble bounded evidence manifest")
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_native_binary_upload(self):
        mutated = self.text.replace(
            "            build/validation/evidence/windows-validation.json",
            "            target/debug/Iris.exe")
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_missing_cleanup(self):
        mutated = self.text.replace("          rm -rf build/validation/ffmpeg-src", "          true")
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_wrong_source_path_layout(self):
        mutated = self.text.replace(
            'Join-Path $env:IRIS_VALIDATION_SOURCE "ffmpeg-8.1.3"',
            "$env:IRIS_VALIDATION_SOURCE", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_wrong_overlay_port_layout(self):
        mutated = self.text.replace('Join-Path $overlay "ffmpeg"',
                                    'Join-Path $overlay "src"', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_missing_triplet_registration(self):
        mutated = self.text.replace("VCPKG_OVERLAY_TRIPLETS", "VCPKG_UNUSED_TRIPLETS", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_missing_dll_runtime_path(self):
        mutated = self.text.replace(
            'Join-Path $env:VCPKG_INSTALLED_DIR "x64-windows-iris-dynamic/bin"',
            'Join-Path $env:VCPKG_INSTALLED_DIR "missing"', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_gh_release_upload(self):
        mutated = self.text.replace(
            "      - name: Assemble bounded evidence manifest",
            "      - name: Publish\n        run: gh release upload v9.9.9 target/debug/Iris.exe\n"
            "      - name: Assemble bounded evidence manifest", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_write_permission_escalation(self):
        mutated = self.text.replace("  contents: read\n", "  contents: write\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_inline_binary_upload_second_step(self):
        mutated = self.text.replace(
            "      - name: Retain bounded JSON evidence only\n",
            "      - name: Ship native binary\n"
            "        uses: actions/upload-artifact@v4\n"
            "        with:\n"
            "          name: native\n"
            "          path: target/debug/Iris.exe\n"
            "      - name: Retain bounded JSON evidence only\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_inline_binary_upload_in_sole_step(self):
        mutated = self.text.replace(
            "          path: |\n"
            "            build/validation/evidence/validation-inputs.json\n"
            "            build/validation/evidence/windows-validation.json\n",
            "          path: target/debug/Iris.exe\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)


class WiringContracts(unittest.TestCase):
    def test_contract_wired_into_ci_and_workflow(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        wf = (ROOT / ".github/workflows/windows-ffmpeg-validation.yml").read_text()
        self.assertIn("python3 scripts/test-windows-ffmpeg-validation-workflow.py", ci)
        self.assertIn("python3 scripts/test-windows-ffmpeg-validation-workflow.py", wf)

    def test_workflow_registered_for_manual_dispatch(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("uses: ./.github/workflows/windows-ffmpeg-validation.yml", ci)


if __name__ == "__main__":
    unittest.main(verbosity=2)
