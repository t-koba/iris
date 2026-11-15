#!/usr/bin/env python3
"""Offline wiring/lifecycle tests; no Windows SDK, media, network or third-party modules."""

import contextlib
import importlib.util
import io
import json
import os
import shutil
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
             width=854, height=480, coded_width=854, coded_height=480, visible_x=0, visible_y=0,
             stride=3416, bytes=1639680, reads=1, rgb_fnv1a64="e46c6403198bdf25",
             timestamp_100ns=0, elapsed_ms=12.5)


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
        self.assertEqual(Path(calls[1][1]).parts[-2:], ("scripts", "fetch-preview-corpus.py"))
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
                       {"coded_width": 853}, {"coded_height": 479}, {"visible_x": 1},
                       {"visible_y": 1}, {"stride": 3415}, {"rgb_fnv1a64": "invalid"}, {"elapsed_ms": float("nan")},
                       {"unexpected": "not retained"}):
            with self.subTest(change=change), self.assertRaises(probe.ProbeError):
                probe.validate_record(json.dumps(FRAME | change), 0, 2048, 16777216, 64)
        failure = dict(ok=False, stage="set_rgb32", hresult="c00d36b4", elapsed_ms=1)
        self.assertEqual(probe.validate_record(json.dumps(failure), 1, 2048, 16777216, 64), failure)
        with self.assertRaises(probe.ProbeError):
            probe.validate_record(json.dumps(failure), 0, 2048, 16777216, 64)
        with self.assertRaises(probe.ProbeError):
            probe.validate_record("[]", 0, 2048, 16777216, 64)

    def test_record_accepts_coded_frame_with_exact_visible_aperture(self):
        padded = FRAME | {"coded_width": 864, "visible_x": 0, "stride": 3456,
                          "bytes": 1658880}
        self.assertEqual(probe.validate_record(json.dumps(padded), 0, 2048, 16777216, 64), padded)
        for change in ({"visible_x": 11}, {"coded_width": 853}, {"stride": 3452}):
            with self.subTest(change=change), self.assertRaises(probe.ProbeError):
                probe.validate_record(json.dumps(padded | change), 0, 2048, 16777216, 64)

    def test_command_suppresses_bounds_and_times_out(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            with self.assertRaisesRegex(probe.ProbeError, "command_log_bound"):
                probe.command([sys.executable, "-c", "print('x' * 200)"], directory, 10, 100)
            with self.assertRaisesRegex(probe.ProbeError, "timeout"):
                probe.command([sys.executable, "-c", "import time; time.sleep(30)"], directory, 0.1, 100)

    def failed_build(self, data, budget=4096):
        paths = []
        output = io.StringIO()
        real_run = probe.run

        def child(argv, cwd, stdout, stderr, timeout, check):
            self.assertEqual(argv[0], "cl.exe")
            self.assertEqual(timeout, 7)
            self.assertFalse(check)
            self.assertEqual(stderr, probe.subprocess.STDOUT)
            paths.append(cwd)
            stdout.write(data)
            return probe.subprocess.CompletedProcess(argv, 1)

        with patch.dict(os.environ, {"IRIS_MF_MAX_LOG_BYTES": str(budget),
                                     "IRIS_MF_BUILD_TIMEOUT_SECONDS": "7"}, clear=True), \
                patch.object(probe.subprocess, "run", side_effect=child), \
                patch.object(probe, "run", side_effect=real_run), \
                patch.object(probe.time, "monotonic", return_value=0), \
                contextlib.redirect_stdout(output):
            self.assertEqual(probe.main(), 1)
        self.assertEqual(len(paths), 1)
        self.assertFalse(paths[0].exists())
        self.assertLessEqual(len(output.getvalue().encode("utf-8")), budget)
        return output.getvalue()

    def test_build_diagnostics_retain_only_codes_and_source_lines(self):
        log = (b"C:\\private\\probe.cpp(42,7): warning C4267: secret-token=abc123\r\n"
               b"C:\\private\\probe.cpp(42,7): warning C4267: duplicate\r\n"
               b"C:\\private\\probe.cpp(43): error C2220: PATH=private\r\n"
               b"cl : Command line warning D9002 : private-token\r\n"
               b"LINK : fatal error LNK1120: \x00\x00\x00\x18ftypmp42\xff\xfe\r\n"
               b"private(99): error X1234: unknown-code\r\n"
               b"error C1083 without diagnostic delimiters\n")
        line = self.failed_build(log)
        self.assertEqual(len(line.splitlines()), 1)
        self.assertEqual(json.loads(line), {
            "ok": False, "stage": "build_failed", "elapsed_ms": 0,
            "diagnostics": [{"code": "C4267", "line": 42},
                            {"code": "C2220", "line": 43},
                            {"code": "D9002"}, {"code": "LNK1120"}]})
        self.assertEqual(line, self.failed_build(log))
        for forbidden in ("private", "probe.cpp", "token", "abc123", "PATH", "ftyp", "X1234"):
            self.assertNotIn(forbidden, line)

    def test_unknown_or_malformed_build_output_contributes_no_text(self):
        log = (b"private(123): note C4267: not a warning\n"
               b"private(456): error C12345: wrong code length\n"
               b"private(789): warning c4267: wrong case\n"
               b"private: error E1234: unknown family\n"
               b"private: warning C4267-token: malformed\n"
               b"TOKEN=abc123; C1083; \x00\x00\x00\x18ftypmp42\xff\xfe\n")
        self.assertEqual(json.loads(self.failed_build(log)),
                         {"ok": False, "stage": "build_failed", "elapsed_ms": 0})

    def test_build_diagnostics_and_child_log_respect_budget(self):
        log = b"".join(f":error C{1000 + i}:\n".encode() for i in range(30))
        full = json.loads(self.failed_build(log))["diagnostics"]
        bounded = json.loads(self.failed_build(log, 512))["diagnostics"]
        self.assertTrue(bounded)
        self.assertLess(len(bounded), len(full))
        self.assertEqual(bounded, full[:len(bounded)])
        # If only the base error fits, omit diagnostics; if even that cannot fit,
        # emit nothing. Neither case can bypass cleanup or the child-log bound.
        small = b":error C1083:"
        self.assertNotIn("diagnostics", json.loads(self.failed_build(small, 64)))
        self.assertEqual(self.failed_build(small, 1), "")
        self.assertEqual(json.loads(self.failed_build(b"x" * 65, 64))["stage"],
                         "build_command_log_bound")
        self.assertEqual(self.failed_build(small, len(small)), "")
        self.assertEqual(json.loads(self.failed_build(b"x" * 4096))["stage"], "build_failed")
        self.assertEqual(json.loads(self.failed_build(b"x" * 4097))["stage"],
                         "build_command_log_bound")

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

    def test_resize_failure_reports_only_bounded_type_dimensions_and_apertures(self):
        missing = {"status": "missing"}
        valid = {"status": "present", "offset_x_value": 0, "offset_x_fraction": 0,
                 "offset_y_value": 0, "offset_y_fraction": 0, "width": 854, "height": 480}
        apertures = {"minimum_display": valid, "geometric": missing}
        failure = dict(ok=False, stage="unexpected_resize", hresult="80004005", elapsed_ms=1,
                       resize={"type": "native", "expected_width": 854, "expected_height": 480,
                               "actual_width": 640, "actual_height": 360})
        self.assertEqual(probe.validate_record(json.dumps(failure), 1, 2048, 16777216, 64), failure)
        output_failure = failure | {"resize": {"type": "output", "expected_width": 854,
                                                  "expected_height": 480, "actual_width": 864,
                                                  "actual_height": 480, "apertures": apertures}}
        self.assertEqual(probe.validate_record(json.dumps(output_failure), 1, 2048, 16777216, 64),
                         output_failure)
        malformed = (
            {"type": "private/path", "expected_width": 854, "expected_height": 480,
             "actual_width": 640, "actual_height": 360},
            {"type": "output", "expected_width": 854, "expected_height": 480,
             "actual_width": 864, "actual_height": 480},
            output_failure["resize"] | {"path": "private/path"},
            output_failure["resize"] | {"apertures": {"minimum_display": missing,
                                                        "geometric": missing, "extra": missing}},
            output_failure["resize"] | {"apertures": {"minimum_display": {"status": "invalid_size",
                                                                                "blob_bytes": 16},
                                                        "geometric": missing}},
            output_failure["resize"] | {"apertures": {"minimum_display": valid | {"raw": "bytes"},
                                                        "geometric": missing}},
            output_failure["resize"] | {"apertures": {"minimum_display": valid | {"width": True},
                                                        "geometric": missing}},
            output_failure["resize"] | {"apertures": {"minimum_display": valid | {"offset_x_value": 32768},
                                                        "geometric": missing}},
        )
        for resize in malformed:
            with self.subTest(resize=resize), self.assertRaises(probe.ProbeError):
                probe.validate_record(json.dumps(failure | {"resize": resize}), 1,
                                      2048, 16777216, 64)

    def test_decode_failure_flags_are_bounded_and_stage_specific(self):
        failure = dict(ok=False, stage="decode_flags", hresult="80004005", elapsed_ms=1)
        for flags in (1, 2, 4, 0x80000000, 2**32 - 1):
            record = failure | {"flags": flags}
            self.assertEqual(probe.validate_record(json.dumps(record), 1, 2048, 16777216, 64), record)
        for change in ({}, {"flags": 0}, {"flags": -1}, {"flags": 2**32},
                       {"flags": True}, {"flags": 1.5}, {"flags": "private/path"},
                       {"flags": 1, "path": "private/path"},
                       {"flags": 1, "stage": "source_open"},
                       {"stage": None}, {"stage": []}, {"hresult": []}):
            with self.subTest(change=change), self.assertRaises(probe.ProbeError):
                probe.validate_record(json.dumps(failure | change), 1, 2048, 16777216, 64)

    def test_flag_failure_logging_and_cleanup_rejects_extra_child_fields(self):
        failure = dict(ok=False, stage="decode_flags", hresult="80004005", elapsed_ms=1, flags=4)
        for extra in ({}, {"path": "private/path", "media": "raw child bytes"}):
            paths = []
            calls = []

            def execute(argv, directory, timeout, max_log):
                paths.append(directory)
                calls.append(argv)
                return (1, json.dumps(failure | extra).encode()) if len(calls) == 3 else (0, b"")

            real_run = probe.run
            output = io.StringIO()
            with patch.dict(os.environ, {}, clear=True), \
                    patch.object(probe, "run", side_effect=lambda: real_run(execute=execute)), \
                    contextlib.redirect_stdout(output):
                self.assertEqual(probe.main(), 1)
            self.assertTrue(all(not path.exists() for path in paths))
            self.assertEqual(len(output.getvalue().splitlines()), 1)
            self.assertLessEqual(len(output.getvalue().encode()), 4096)
            record = json.loads(output.getvalue())
            if extra:
                self.assertEqual(record["stage"], "decode_invalid_failure")
                self.assertNotIn("flags", record)
            else:
                self.assertEqual(record, failure | {"fixture_sha256": record["fixture_sha256"]})
            for forbidden in ("private", "path", "media", "raw child bytes"):
                self.assertNotIn(forbidden, output.getvalue())

    def test_native_notification_validation_with_fake_media_types(self):
        # Execute the actual C++ validators, not a Python copy of their policy.
        # This optional SDK-free harness cannot establish Windows compatibility.
        compiler = shutil.which("c++") or shutil.which("g++")
        if not compiler:
            self.skipTest("SDK-free C++ compiler unavailable; source contracts still run")
        source = (HERE / "probe.cpp").read_text()
        validators = source[source.index("enum class ApertureStatus"):source.index("struct Runtime {")]
        harness = r'''
#include <cassert>
#include <cstdint>
#include <cstring>
#include <string>
#include <initializer_list>
using DWORD = uint32_t;
using UINT32 = uint32_t;
using UINT8 = uint8_t;
using LONG = int32_t;
using HRESULT = int32_t;
using REFGUID = int;
constexpr HRESULT E_FAIL = -1, E_INVALIDARG = -2, MF_E_ATTRIBUTENOTFOUND = -3;
bool FAILED(HRESULT hr) { return hr < 0; }
struct GUID { int Data1; };
struct MFOffset { int16_t value = 0; uint16_t fract = 0; };
struct MFVideoArea { MFOffset OffsetX, OffsetY; struct { LONG cx = 0, cy = 0; } Area; };
bool operator!=(GUID a, GUID b) { return a.Data1 != b.Data1; }
constexpr GUID MFMediaType_Video{1}, MFVideoFormat_RGB32{2}, MFVideoFormat_H264{3};
constexpr int MF_MT_MAJOR_TYPE = 1, MF_MT_SUBTYPE = 2, MF_MT_FRAME_SIZE = 3,
              MF_MT_DEFAULT_STRIDE = 4, MF_MT_MINIMUM_DISPLAY_APERTURE = 5,
              MF_MT_GEOMETRIC_APERTURE = 6;
constexpr DWORD kFirstVideoStream = 7, kCurrentType = 0xffffffff;
constexpr DWORD MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED = 0x10,
                MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED = 0x20,
                MF_SOURCE_READERF_STREAMTICK = 0x100;
struct IMFMediaType {
    GUID major = MFMediaType_Video, subtype = MFVideoFormat_RGB32;
    UINT32 width = 854, height = 480, stride = 3416;
    HRESULT status = 0, stride_status = 0;
    MFVideoArea minimum_display{}, geometric{};
    UINT32 minimum_bytes = 0, geometric_bytes = 0;
    HRESULT minimum_size_status = MF_E_ATTRIBUTENOTFOUND, geometric_size_status = MF_E_ATTRIBUTENOTFOUND;
    HRESULT minimum_blob_status = 0, geometric_blob_status = 0;
    HRESULT GetGUID(int key, GUID* value) {
        *value = key == MF_MT_MAJOR_TYPE ? major : subtype;
        return status;
    }
    HRESULT GetUINT32(int, UINT32* value) { *value = stride; return stride_status; }
    HRESULT GetBlobSize(int key, UINT32* bytes) {
        if (key == MF_MT_MINIMUM_DISPLAY_APERTURE) { *bytes = minimum_bytes; return minimum_size_status; }
        *bytes = geometric_bytes; return geometric_size_status;
    }
    HRESULT GetBlob(int key, UINT8* data, UINT32 capacity, UINT32* bytes) {
        const auto status = key == MF_MT_MINIMUM_DISPLAY_APERTURE ? minimum_blob_status : geometric_blob_status;
        const auto size = key == MF_MT_MINIMUM_DISPLAY_APERTURE ? minimum_bytes : geometric_bytes;
        const auto& area = key == MF_MT_MINIMUM_DISPLAY_APERTURE ? minimum_display : geometric;
        if (FAILED(status)) return status;
        if (capacity < sizeof(area) || size != sizeof(area)) return E_FAIL;
        std::memcpy(data, &area, sizeof(area)); *bytes = sizeof(area); return 0;
    }
};
HRESULT MFGetAttributeSize(IMFMediaType* type, int, UINT32* w, UINT32* h) {
    *w = type->width; *h = type->height; return type->status;
}
HRESULT MFGetStrideForBitmapInfoHeader(int, UINT32 w, LONG* stride) {
    *stride = static_cast<LONG>(w * 4); return 0;
}
template<class T> struct ComPtr {
    T* value = nullptr;
    T* Get() { return value; }
    T** operator&() { return &value; }
};
struct IMFSourceReader {
    IMFMediaType native, output;
    int native_queries = 0, output_queries = 0;
    HRESULT native_status = 0, output_status = 0;
    IMFSourceReader() { native.subtype = MFVideoFormat_H264; }
    HRESULT GetNativeMediaType(DWORD stream, DWORD index, IMFMediaType** value) {
        assert(stream == kFirstVideoStream && index == kCurrentType);
        ++native_queries; *value = &native; return native_status;
    }
    HRESULT GetCurrentMediaType(DWORD stream, IMFMediaType** value) {
        assert(stream == kFirstVideoStream);
        ++output_queries; *value = &output; return output_status;
    }
};
'''
        cases = r'''
int main() {
    const Limits limits{2048, 16777216, 64};
    const Layout input{854, 480, 3416, 1639680, {0, 0, 854, 480}};
    for (DWORD flags : {0u, 0x100u, 0x10u, 0x20u, 0x30u, 0x110u, 0x120u, 0x130u}) {
        IMFSourceReader reader;
        reader.output.stride = 3424; // Changed but still bounded padding is valid.
        Layout frame = input;
        validate_read(&reader, flags, limits, input, frame);
        const bool changed = (flags & 0x30) != 0;
        assert(reader.native_queries == changed && reader.output_queries == changed);
        assert(frame.stride == (changed ? 3424u : 3416u));
        assert(frame.bytes == frame.stride * frame.height);
    }
    // Every forbidden bit, alone or combined with notifications: ERROR=1,
    // EOF=2, NEWSTREAM=4, ALLEFFECTSREMOVED=0x200 and all unknown bits.
    for (unsigned bit = 0; bit < 32; ++bit) {
        DWORD flag = DWORD(1) << bit;
        if (flag & 0x130) continue;
        for (DWORD flags : {flag, flag | 0x30}) {
            IMFSourceReader reader;
            Layout frame = input;
            try { validate_read(&reader, flags, limits, input, frame); return 1; }
            catch (const Failure& f) {
                assert(std::strcmp(f.stage, "decode_flags") == 0 && f.flags == flags);
                assert(reader.native_queries == 0 && reader.output_queries == 0);
            }
        }
    }
    for (DWORD flags : {0x10u, 0x20u, 0x30u}) {
        for (int invalid = 0; invalid < 18; ++invalid) {
            IMFSourceReader reader;
            switch (invalid) {
                case 0: reader.native.subtype = MFVideoFormat_RGB32; break;
                case 1: reader.output.subtype = MFVideoFormat_H264; break;
                case 2: reader.native.width = 853; break;
                case 3: reader.native.height = 481; break;
                case 4: reader.output.width = 853; break;
                case 5: reader.output.height = 481; break;
                case 6: reader.native.width = 0; break;
                case 7: reader.output.height = 2049; break;
                case 8: reader.output.stride = 3415; break;
                case 9: reader.output.stride = 16777217; break;
                case 10: reader.output.stride = 40000; break;
                case 11: reader.native.major = GUID{99}; break;
                case 12: reader.output.major = GUID{99}; break;
                case 13: reader.native_status = E_FAIL; break;
                case 14: reader.output_status = E_FAIL; break;
                case 15: reader.output.status = E_FAIL; break;
                case 16: reader.native.status = E_FAIL; break;
                case 17: reader.output.stride_status = E_FAIL; break;
            }
            Layout frame = input;
            try { validate_read(&reader, flags, limits, input, frame); return 2; }
            catch (const Failure& f) {
                assert(f.flags == 0 && frame.stride == input.stride);
                if (invalid >= 2 && invalid <= 5) {
                    const char* expected_type = invalid <= 3 ? "native" : "output";
                    const UINT32 expected_width = input.width;
                    const UINT32 expected_height = input.height;
                    const UINT32 actual_width = (invalid == 2 || invalid == 4) ? 853 : 854;
                    const UINT32 actual_height = (invalid == 3 || invalid == 5) ? 481 : 480;
                    assert(std::strcmp(f.stage, "unexpected_resize") == 0);
                    assert(std::strcmp(f.resize_type, expected_type) == 0);
                    assert(f.expected_width == expected_width && f.expected_height == expected_height);
                    assert(f.actual_width == actual_width && f.actual_height == actual_height);
                }
            }
        }
    }
    // Coded-width padding is accepted only when both exact display apertures prove the visible frame.
    {
        IMFSourceReader reader;
        reader.output.width = 864;
        reader.output.stride = 3456;
        reader.output.minimum_bytes = reader.output.geometric_bytes = sizeof(MFVideoArea);
        reader.output.minimum_size_status = reader.output.geometric_size_status = 0;
        reader.output.minimum_display.Area.cx = reader.output.geometric.Area.cx = 854;
        reader.output.minimum_display.Area.cy = reader.output.geometric.Area.cy = 480;
        Layout frame = input;
        validate_read(&reader, 0x30, limits, input, frame);
        assert(frame.width == 864 && frame.height == 480);
        assert(frame.visible.x == 0 && frame.visible.y == 0);
        assert(frame.visible.width == 854 && frame.visible.height == 480);
        assert(frame.stride == 3456 && frame.bytes == 3456 * 480);
    }
    // Output mismatch diagnostics expose only fixed numeric apertures, never raw media bytes.
    {
        IMFSourceReader reader;
        reader.output.width = 864;
        reader.output.stride = 3456;
        reader.output.minimum_bytes = sizeof(MFVideoArea);
        reader.output.minimum_size_status = 0;
        reader.output.minimum_display.OffsetX.value = 1;
        reader.output.minimum_display.OffsetX.fract = 2;
        reader.output.minimum_display.OffsetY.value = -3;
        reader.output.minimum_display.OffsetY.fract = 4;
        reader.output.minimum_display.Area.cx = 854;
        reader.output.minimum_display.Area.cy = 480;
        try { checked_output(&reader, limits, input); return 3; }
        catch (const Failure& f) {
            assert(std::strcmp(f.stage, "unexpected_resize") == 0 && f.has_apertures);
            assert(f.minimum_display_aperture.status == ApertureStatus::present);
            assert(f.minimum_display_aperture.offset_x_value == 1);
            assert(f.minimum_display_aperture.offset_x_fraction == 2);
            assert(f.minimum_display_aperture.offset_y_value == -3);
            assert(f.minimum_display_aperture.offset_y_fraction == 4);
            assert(f.minimum_display_aperture.width == 854 && f.minimum_display_aperture.height == 480);
            assert(f.geometric_aperture.status == ApertureStatus::missing);
        }
        reader.output.minimum_bytes = 1;
        assert(read_aperture(&reader.output, MF_MT_MINIMUM_DISPLAY_APERTURE).status ==
               ApertureStatus::invalid_size);
        reader.output.minimum_bytes = sizeof(MFVideoArea);
        reader.output.minimum_blob_status = E_FAIL;
        assert(read_aperture(&reader.output, MF_MT_MINIMUM_DISPLAY_APERTURE).status ==
               ApertureStatus::unreadable);
    }
    // Signed and absent strides retain the same bounds as initial negotiation.
    for (bool absent : {false, true}) {
        IMFSourceReader reader;
        reader.output.stride = static_cast<UINT32>(-3416);
        if (absent) reader.output.stride_status = MF_E_ATTRIBUTENOTFOUND;
        Layout frame = input;
        validate_read(&reader, 0x30, limits, input, frame);
        assert(frame.stride == 3416 && frame.bytes == input.bytes);
    }
}
'''
        with tempfile.TemporaryDirectory(prefix="iris-mf-contract-") as temp:
            directory = Path(temp)
            (directory / "contract.cpp").write_text(harness + validators + cases)
            executable = directory / ("contract.exe" if os.name == "nt" else "contract")
            code, _ = probe.command([compiler, "-std=c++17", "-Wall", "-Wextra", "-Werror",
                                     "contract.cpp", "-o", str(executable)], directory,
                                    probe.setting("IRIS_MF_BUILD_TIMEOUT_SECONDS", 120),
                                    probe.setting("IRIS_MF_MAX_LOG_BYTES", 4096))
            self.assertEqual(code, 0, "SDK-free validator harness did not compile (output suppressed)")
            code, _ = probe.command([str(executable)], directory,
                                    probe.setting("IRIS_MF_DECODE_TIMEOUT_SECONDS", 30),
                                    probe.setting("IRIS_MF_MAX_LOG_BYTES", 4096))
            self.assertEqual(code, 0, "SDK-free validator cases failed (output suppressed)")

    def test_native_checks_before_copy_and_system_libraries_only(self):
        source = (HERE / "probe.cpp").read_text()
        self.assertLess(source.index('layout(native.Get()'), source.index('reader->ReadSample('))
        self.assertLess(source.index('validate_read(reader.Get(), flags'), source.index('if (!sample)'))
        self.assertIn('if (flags & ~allowed)', source)
        self.assertIn('if (flags & changes) frame = checked_output(reader, limits, input)', source)
        self.assertIn('if (failure.flags) std::printf(",\\\"flags\\\":%lu", failure.flags)', source)
        self.assertLess(source.index('total > limits.bytes'), source.index('sample->ConvertToContiguousBuffer('))
        for contract in ("MFCreateSourceReaderFromURL", "MFVideoFormat_H264", "MFVideoFormat_RGB32",
                         "MF_SOURCE_READER_CURRENT_TYPE_INDEX", "dimension_bound", "invalid_buffer",
                         "CoUninitialize", "MFShutdown", "buffer->Unlock()"):
            self.assertIn(contract, source)


if __name__ == "__main__":
    unittest.main()
