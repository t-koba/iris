"""Create one bounded, path-sanitized report; never retain prototype payloads."""
import hashlib
import json
import os
from pathlib import Path
import re
import sys

INPUTS = (
    ("ffmpeg_source", "target/prototype-windows-ffmpeg/ffmpeg-source.json", "json"),
    ("ffmpeg_build", "target/prototype-windows-ffmpeg/ffmpeg-build.json", "json"),
    ("ffmpeg_buildconf", "target/prototype-windows-ffmpeg/ffmpeg-buildconf.txt", "text"),
    ("ffmpeg_version", "target/prototype-windows-ffmpeg/ffmpeg-version.txt", "text"),
    ("ffmpeg_overlay", "target/prototype-windows-ffmpeg/overlay.json", "json"),
    ("pdfium_provenance", "target/prototype-windows-pdfium/PROVENANCE.json", "json"),
    ("pe_import_closure", "target/prototype-windows-app/IMPORT-CLOSURE.json", "json"),
    ("prototype_app_manifest", "target/prototype-windows-app/PROTOTYPE-APP.json", "json"),
    ("installed_smoke", "target/prototype-windows-installer/smoke-result.json", "json"),
)
PRIVATE_KEYS = {"path", "root", "dumpbin", "app_dir", "install_dir", "video_tool", "workspace"}
ABSOLUTE_PATH = re.compile(r"(^|[\s\"'=])((?:[A-Za-z]:[\\/]|\\\\|/(?!/))[^\s\"',;]+)")


def _sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _private_key(key):
    name = str(key).lower()
    return name in PRIVATE_KEYS or name.endswith(("_path", "_dir", "_root"))


def _sanitize_string(value):
    return ABSOLUTE_PATH.sub(lambda match: match.group(1) + "<redacted-path>", value)


def sanitize(value):
    if isinstance(value, dict):
        return {key: sanitize(item) for key, item in value.items() if not _private_key(key)}
    if isinstance(value, list):
        return [sanitize(item) for item in value]
    if isinstance(value, str):
        return _sanitize_string(value)
    return value


def _encode(report):
    return json.dumps(report, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def build_report(root, max_bytes):
    root = Path(root)
    entries = {}
    complete = True
    for name, relative, kind in INPUTS:
        path = root / relative
        if not path.is_file():
            entries[name] = {"status": "missing"}
            complete = False
            continue
        size = path.stat().st_size
        if size > max_bytes:
            entries[name] = {"bytes": size, "status": "omitted_input_limit"}
            complete = False
            continue
        digest = _sha256_file(path)
        base = {"bytes": size, "sha256": digest}
        raw = path.read_bytes()
        try:
            if kind == "json":
                data = sanitize(json.loads(raw.decode("utf-8-sig")))
            else:
                data = _sanitize_string(raw.decode("utf-8-sig"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            entries[name] = {**base, "status": "invalid_report"}
            complete = False
            continue
        entries[name] = {**base, "status": "included", "data": data}

    report = {"schema": 1, "max_bytes": max_bytes, "complete": complete, "reports": entries}
    encoded = _encode(report)
    if len(encoded) > max_bytes:
        report["complete"] = False
        for entry in report["reports"].values():
            if "data" in entry:
                del entry["data"]
                entry["status"] = "omitted_total_limit"
        encoded = _encode(report)
    if len(encoded) > max_bytes:
        raise ValueError("configured report bound is too small for the bounded manifest")
    return report, encoded


def main():
    try:
        max_bytes = int(os.environ["IRIS_PROTOTYPE_AUDIT_REPORT_MAX_BYTES"])
        if max_bytes <= 0:
            raise ValueError
    except (KeyError, ValueError):
        print("IRIS_PROTOTYPE_AUDIT_REPORT_MAX_BYTES must be a positive integer", file=sys.stderr)
        return 2

    root = Path(__file__).resolve().parents[2]
    output = root / "target/prototype-audit-report.json"
    try:
        report, encoded = build_report(root, max_bytes)
        output.parent.mkdir(parents=True, exist_ok=True)
        temporary = output.with_name(output.name + ".tmp")
        temporary.write_bytes(encoded)
        temporary.replace(output)
    except (OSError, ValueError) as error:
        print(f"could not create bounded prototype audit report: {type(error).__name__}", file=sys.stderr)
        return 1

    print(f"prototype audit report: {len(encoded)} bytes (limit {max_bytes})")
    if not report["complete"]:
        print("prototype evidence is incomplete; inspect the bounded report and job logs", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
