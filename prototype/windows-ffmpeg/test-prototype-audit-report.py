"""Protect report-only retention, bounds, and path sanitization."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("prototype_audit_report", ROOT / "create-prototype-audit-report.py")
reporter = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reporter)


class PrototypeAuditReportTests(unittest.TestCase):
    def _write_all_inputs(self, root, payload=None):
        for name, relative, kind in reporter.INPUTS:
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            if payload is not None and name == "pe_import_closure":
                path.write_text(json.dumps(payload), encoding="utf-8")
            elif kind == "json":
                path.write_text(json.dumps({"sha256": "a" * 64, "file": "Iris.exe"}), encoding="utf-8")
            else:
                path.write_text("configuration: --enable-shared\n", encoding="utf-8")

    def test_includes_only_sanitized_reports(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._write_all_inputs(root, {
                "app_dir": "D:\\runner\\workspace\\app",
                "dumpbin": "C:\\Program Files\\VS\\dumpbin.exe",
                "files": [{"file": "avcodec-61.dll", "path": "D:\\runner\\workspace\\app\\avcodec-61.dll", "sha256": "b" * 64}],
                "url": "https://example.invalid/source.tar.xz",
                "note": "failed at /tmp/private/build",
            })
            report, encoded = reporter.build_report(root, 262144)
            text = encoded.decode("utf-8")
            self.assertTrue(report["complete"])
            self.assertLessEqual(len(encoded), 262144)
            self.assertNotIn("D:\\runner", text)
            self.assertNotIn("/tmp/private", text)
            self.assertNotIn("app_dir", text)
            self.assertNotIn("dumpbin", text)
            self.assertIn("https://example.invalid/source.tar.xz", text)
            self.assertIn("avcodec-61.dll", text)

    def test_missing_and_oversize_inputs_are_explicit_and_bounded(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._write_all_inputs(root)
            missing = root / reporter.INPUTS[0][1]
            missing.unlink()
            too_large = root / reporter.INPUTS[1][1]
            too_large.write_text("x" * 8193, encoding="utf-8")
            report, encoded = reporter.build_report(root, 8192)
            self.assertFalse(report["complete"])
            self.assertEqual(report["reports"]["ffmpeg_source"]["status"], "missing")
            self.assertEqual(report["reports"]["ffmpeg_build"]["status"], "omitted_input_limit")
            self.assertLessEqual(len(encoded), 8192)

    def test_total_report_limit_falls_back_to_hashes_not_truncation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._write_all_inputs(root)
            for name, relative, kind in reporter.INPUTS:
                path = root / relative
                if kind == "text":
                    path.write_text("x" * 20000, encoding="utf-8")
                else:
                    path.write_text(json.dumps({"payload": "x" * 20000}), encoding="utf-8")
            report, encoded = reporter.build_report(root, 65536)
            self.assertFalse(report["complete"])
            self.assertLessEqual(len(encoded), 65536)
            self.assertTrue(all("data" not in entry for entry in report["reports"].values()))
            self.assertTrue(all(entry["status"] == "omitted_total_limit" for entry in report["reports"].values()))


if __name__ == "__main__":
    unittest.main()
