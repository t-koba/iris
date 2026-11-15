#!/usr/bin/env python3
"""Build/run the test-only Windows SDK probe; retain JSON only, never media bytes."""

import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]


class ProbeError(Exception):
    def __init__(self, stage, diagnostics=None):
        super().__init__(stage)
        self.diagnostics = diagnostics or []


def build_diagnostics(data):
    # MSVC compiler (C), command-line (D), and linker (LNK) code syntax.
    # Match whole diagnostic lines, but retain neither origin nor message bytes.
    pattern = (rb"[^\r\n]*?(?:\(([0-9]+)(?:, *[0-9]+)?\))? *: *"
               rb"(?:Command line )?(?:fatal error|error|warning) +"
               rb"(C[0-9]{4}|D[0-9]{4}|LNK[0-9]{4}) *:[^\r\n]*")
    diagnostics = []
    seen = set()
    for line in data.splitlines():
        match = re.fullmatch(pattern, line)
        if not match:
            continue
        code = match[2].decode("ascii")
        try:
            source_line = int(match[1]) if match[1] else None
        except ValueError:
            source_line = None
        key = (code, source_line)
        if key not in seen:
            seen.add(key)
            diagnostic = {"code": code}
            if source_line is not None:
                diagnostic["line"] = source_line
            diagnostics.append(diagnostic)
    return diagnostics


def setting(name, default):
    value = int(os.environ.get(name, default))
    if value <= 0:
        raise ProbeError("configuration")
    return value


def command(argv, directory, timeout, max_log):
    # All child output stays in the same disposable directory, not CI logs.
    with (directory / "command.log").open("w+b") as output:
        try:
            result = subprocess.run(argv, cwd=directory, stdout=output,
                                    stderr=subprocess.STDOUT, timeout=timeout, check=False)
        except subprocess.TimeoutExpired as error:
            raise ProbeError("timeout") from error
        output.seek(0)
        data = output.read(max_log + 1)
        if len(data) > max_log:
            raise ProbeError("command_log_bound")
        return result.returncode, data


def validate_record(data, code, dimension, max_bytes, reads):
    record = json.loads(data)
    if not isinstance(record, dict) or type(record.get("ok")) is not bool:
        raise ProbeError("invalid_record")
    elapsed = record.get("elapsed_ms")
    if type(elapsed) not in (int, float) or not math.isfinite(elapsed) or elapsed < 0:
        raise ProbeError("invalid_elapsed")
    if not record["ok"]:
        fields = {"ok", "stage", "hresult", "elapsed_ms"}
        if record.get("stage") == "decode_flags":
            fields.add("flags")
            # DWORD is an intrinsic Windows ABI bound; bool is not an integer.
            if type(record.get("flags")) is not int or not 0 < record["flags"] < 2**32:
                raise ProbeError("invalid_failure")
        if (code == 0 or set(record) != fields
                or type(record.get("stage")) is not str
                or not re.fullmatch(r"[a-z][a-z0-9_]{0,39}", record["stage"])
                or type(record.get("hresult")) is not str
                or not re.fullmatch(r"[0-9a-f]{8}", record["hresult"])):
            raise ProbeError("invalid_failure")
        return record
    if (code != 0 or set(record) != {"ok", "backend", "codec", "pixel_format", "width", "height",
                                    "stride", "bytes", "reads", "rgb_fnv1a64", "timestamp_100ns", "elapsed_ms"}
            or record["backend"] != "mf-source-reader" or record["codec"] != "h264"
            or record["pixel_format"] != "rgb32"):
        raise ProbeError("invalid_success")
    for key in ("width", "height", "stride", "bytes", "reads", "timestamp_100ns"):
        if type(record[key]) is not int:
            raise ProbeError("invalid_integer")
    if (not 0 < record["width"] <= dimension or not 0 < record["height"] <= dimension
            or not record["width"] * 4 <= record["stride"] <= max_bytes
            or not record["stride"] * record["height"] <= record["bytes"] <= max_bytes
            or not 0 < record["reads"] <= reads
            or not -(2**63) <= record["timestamp_100ns"] < 2**63
            or not re.fullmatch(r"[0-9a-f]{16}", record["rgb_fnv1a64"])):
        raise ProbeError("invalid_frame")
    return record


def run(execute=command, temporary=tempfile.TemporaryDirectory):
    dimension = setting("IRIS_MF_MAX_DIMENSION", 2048)
    max_bytes = setting("IRIS_MF_MAX_FRAME_BYTES", 16777216)
    reads = setting("IRIS_MF_MAX_READS", 64)
    max_log = setting("IRIS_MF_MAX_LOG_BYTES", 4096)
    build_timeout = setting("IRIS_MF_BUILD_TIMEOUT_SECONDS", 120)
    fetch_timeout = setting("IRIS_MF_FETCH_TIMEOUT_SECONDS", 300)
    decode_timeout = setting("IRIS_MF_DECODE_TIMEOUT_SECONDS", 30)
    manifest = ROOT / "tests/preview-corpus/fixtures.json"
    fixture = next(entry for entry in json.loads(manifest.read_text(encoding="utf-8"))["files"]
                   if entry["filename"] == "bunny.mp4")
    stage = "build"
    try:
        with temporary(prefix="iris-mf-", dir=os.environ.get("IRIS_MF_TEMP_ROOT")) as temp:
            directory = Path(temp)
            executable = directory / "probe.exe"
            source = ROOT / "prototype/windows-media-foundation/probe.cpp"
            code, data = execute(["cl.exe", "/nologo", "/EHsc", "/std:c++17", "/W4", "/WX",
                               str(source), f"/Fe:{executable}", "/Fo:probe.obj", "/Fd:probe.pdb",
                               "/link", "mfreadwrite.lib", "mfplat.lib", "mfuuid.lib", "ole32.lib"],
                              directory, build_timeout, max_log)
            if code:
                raise ProbeError("failed", build_diagnostics(data))
            stage = "fetch"
            code, _ = execute([sys.executable, str(ROOT / "scripts/fetch-preview-corpus.py"),
                               "--manifest", str(manifest), "--output", str(directory / "corpus")],
                              directory, fetch_timeout, max_log)
            if code:
                raise ProbeError("failed")
            stage = "decode"
            # No directory scan: the decoder receives ONLY the pinned MP4.
            code, data = execute([str(executable), str(directory / "corpus/bunny.mp4"),
                                  str(dimension), str(max_bytes), str(reads)],
                                 directory, decode_timeout, max_log)
            record = validate_record(data, code, dimension, max_bytes, reads)
            record["fixture_sha256"] = fixture["sha256"]
            line = json.dumps(record, separators=(",", ":"), allow_nan=False)
            if len(line.encode("utf-8")) + 1 > max_log:
                raise ProbeError("record_bound")
        # Cleanup is complete before a result is made visible.
        return line, 0 if record["ok"] else 1
    except ProbeError as error:
        raise ProbeError(f"{stage}_{error}", error.diagnostics) from error
    except (OSError, ValueError, KeyError, TypeError, StopIteration) as error:
        raise ProbeError(f"{stage}_invalid_or_unavailable") from error


def main():
    started = time.monotonic()
    try:
        line, code = run()
    except (ProbeError, ValueError, OSError, KeyError, StopIteration) as error:
        # Never print exceptions/child output: these may contain paths or bytes.
        stage = str(error) if isinstance(error, ProbeError) else "configuration"
        record = {"ok": False, "stage": stage,
                  "elapsed_ms": round((time.monotonic() - started) * 1000, 3)}
        line = json.dumps(record, separators=(",", ":"))
        if isinstance(error, ProbeError) and error.diagnostics:
            # Retain a deterministic prefix that fits the same total JSON budget.
            # No separate count limit; the configured byte budget owns retention.
            try:
                budget = setting("IRIS_MF_MAX_LOG_BYTES", 4096)
            except (ProbeError, ValueError):
                return 1
            size = len(line.encode("utf-8")) + len(',"diagnostics":[]') + 1
            retained = []
            for diagnostic in error.diagnostics:
                extra = len(json.dumps(diagnostic, separators=(",", ":"))) + bool(retained)
                if size + extra > budget:
                    break
                retained.append(diagnostic)
                size += extra
            if retained:
                record["diagnostics"] = retained
                line = json.dumps(record, separators=(",", ":"))
        code = 1
    # Even an error record must respect the configured retained-log budget.
    # An impossibly small/invalid budget fails closed without printing a line.
    try:
        if len(line.encode("utf-8")) + 1 > setting("IRIS_MF_MAX_LOG_BYTES", 4096):
            return 1
    except (ProbeError, ValueError):
        return 1
    print(line)
    return code


if __name__ == "__main__":
    sys.exit(main())
