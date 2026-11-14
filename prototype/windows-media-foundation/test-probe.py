#!/usr/bin/env python3
"""Offline wiring/lifecycle tests; no Windows SDK, media, network or third-party modules."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("mf_probe", HERE / "run.py")
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)

FRAME = dict(ok=True, backend="mf-source-reader", codec="h264", pixel_format="rgb32",
             width=854, height=480, stride=3416, bytes=1639680, reads=1,
             rgb_fnv1a64="e46c6403198bdf25", timestamp_100ns=0, elapsed_ms=12.5)


class ProbeTests(unittest.TestCase):
    def test_workflow_is_manual_ci_only_and_cleanup_only(self):
        workflow = (probe.ROOT / ".github/workflows/windows-media-foundation-probe.yml").read_text()
        triggers = workflow.split("\non:\n", 1)[1].split("\npermissions:", 1)[0]
        self.assertEqual(triggers.strip(), "workflow_call:")
        self.assertIn("runs-on: windows-2022", workflow)
        self.assertIn("IRIS_PREVIEW_CORPUS_MAX_BYTES: '40000000'", workflow)
        self.assertIn("python prototype/windows-media-foundation/test-probe.py", workflow)
        self.assertIn("python prototype/windows-media-foundation/run.py", workflow)
        self.assertIn("arch: x64", workflow)
        self.assertIn("if: always()", workflow)
        self.assertNotIn("runner.temp", workflow)
        self.assertIn("Join-Path $env:RUNNER_TEMP", workflow)
        self.assertIn('"IRIS_MF_TEMP_ROOT=$root" >> $env:GITHUB_ENV', workflow)
        caller = (probe.ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("windows-media-foundation-probe:\n    if: github.event_name == 'workflow_dispatch'\n"
                      "    permissions:\n      contents: read\n"
                      "    uses: ./.github/workflows/windows-media-foundation-probe.yml", caller)
        self.assertIn("finally {", workflow)
        self.assertIn("Remove-Item -LiteralPath $env:IRIS_MF_TEMP_ROOT", workflow)
        self.assertIn("Remove-Item -LiteralPath $root", workflow)
        self.assertEqual(workflow.count("Remove-Item -LiteralPath"), 2)
        for forbidden in ("upload-artifact", "workflow_dispatch:", "windows-minimal", "cargo ",
                          "package-", "ffmpeg", "pdfium", "release.yml"):
            self.assertNotIn(forbidden, workflow)

    def simulate(self, fail=None):
        calls = []
        paths = []

        def execute(argv, directory, timeout, max_log):
            paths.append(directory)
            calls.append(argv)
            self.assertGreater(timeout, 0)
            self.assertGreater(max_log, 0)
            (directory / "output.bin").write_bytes(b"temporary output")
            if len(calls) == 2:
                corpus = directory / "corpus"
                self.assertFalse(corpus.exists())
                corpus.mkdir()
                (corpus / "bunny.mp4").write_bytes(b"mock fixture")
            if fail == len(calls):
                return 1, b"private paths and child diagnostics must not be logged"
            if fail == "timeout" and len(calls) == 3:
                raise probe.ProbeError("timeout")
            if len(calls) == 3:
                return 0, json.dumps(FRAME).encode()
            return 0, b"suppressed child output"

        with patch.dict(os.environ, {}, clear=True):
            if fail:
                with self.assertRaises(probe.ProbeError) as caught:
                    probe.run(execute=execute)
                self.assertNotIn("private", str(caught.exception))
            else:
                line, code = probe.run(execute=execute)
                self.assertEqual(code, 0)
                self.assertEqual(json.loads(line)["width"], 854)
                self.assertLess(len(line), 4096)
                self.assertIn("fixture_sha256", json.loads(line))
        self.assertTrue(paths)
        self.assertTrue(all(not path.exists() for path in paths))
        return calls

    def test_build_fetch_and_only_mp4_decode_with_cleanup(self):
        calls = self.simulate()
        self.assertEqual(calls[0][0], "cl.exe")
        self.assertIn("mfreadwrite.lib", calls[0])
        self.assertTrue(calls[1][1].endswith("scripts/fetch-preview-corpus.py"))
        self.assertIn("--manifest", calls[1])
        self.assertEqual(len(calls[2]), 5)
        self.assertEqual(Path(calls[2][1]).name, "bunny.mp4")
        self.assertEqual(calls[2][2:], ["2048", "16777216", "64"])

    def test_cleanup_on_each_failure_and_timeout(self):
        for failure in (1, 2, 3, "timeout"):
            with self.subTest(failure=failure):
                self.simulate(failure)

    def test_record_validates_bounds_and_failure_hresult(self):
        for change in ({"width": 0}, {"width": 2049}, {"height": -1}, {"stride": 1},
                       {"bytes": 16777217}, {"bytes": 10}, {"reads": 65}, {"width": True},
                       {"rgb_fnv1a64": "invalid"}, {"elapsed_ms": float("nan")},
                       {"unexpected": "not retained"}):
            with self.subTest(change=change), self.assertRaises(probe.ProbeError):
                probe.validate_record(json.dumps(FRAME | change), 0, 2048, 16777216, 64)
        failure = dict(ok=False, stage="set_rgb32", hresult="c00d36b4", elapsed_ms=1)
        self.assertEqual(probe.validate_record(json.dumps(failure), 1, 2048, 16777216, 64), failure)
        with self.assertRaises(probe.ProbeError):
            probe.validate_record(json.dumps(failure), 0, 2048, 16777216, 64)
        with self.assertRaises(probe.ProbeError):
            probe.validate_record("[]", 0, 2048, 16777216, 64)

    def test_command_suppresses_bounds_and_times_out(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            with self.assertRaisesRegex(probe.ProbeError, "command_log_bound"):
                probe.command([sys.executable, "-c", "print('x' * 200)"], directory, 10, 100)
            with self.assertRaisesRegex(probe.ProbeError, "timeout"):
                probe.command([sys.executable, "-c", "import time; time.sleep(30)"], directory, 0.1, 100)

    def test_main_emits_one_sanitized_failure_line(self):
        output = io.StringIO()
        with patch.object(probe, "run", side_effect=OSError("private/path")), contextlib.redirect_stdout(output):
            self.assertEqual(probe.main(), 1)
        self.assertEqual(len(output.getvalue().splitlines()), 1)
        self.assertNotIn("private", output.getvalue())
        self.assertFalse(json.loads(output.getvalue())["ok"])

    def test_error_line_respects_log_budget(self):
        output = io.StringIO()
        with patch.dict(os.environ, {"IRIS_MF_MAX_LOG_BYTES": "1"}), \
                patch.object(probe, "run", side_effect=OSError("private/path")), \
                contextlib.redirect_stdout(output):
            self.assertEqual(probe.main(), 1)
        self.assertEqual(output.getvalue(), "")

    def test_native_checks_before_copy_and_system_libraries_only(self):
        source = (HERE / "probe.cpp").read_text()
        self.assertLess(source.index('layout(native.Get()'), source.index('reader->ReadSample('))
        self.assertLess(source.index('total > limits.bytes'), source.index('sample->ConvertToContiguousBuffer('))
        for contract in ("MFCreateSourceReaderFromURL", "MFVideoFormat_H264", "MFVideoFormat_RGB32",
                         "MF_SOURCE_READERF_ENDOFSTREAM", "dimension_bound", "invalid_buffer",
                         "CoUninitialize", "MFShutdown", "buffer->Unlock()"):
            self.assertIn(contract, source)


if __name__ == "__main__":
    unittest.main()
