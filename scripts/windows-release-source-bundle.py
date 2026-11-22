#!/usr/bin/env python3
"""Build bounded, deterministic source/notice evidence for Windows releases.

This runs on the validating Windows runner after the signature-verified FFmpeg
source, the derived vcpkg overlay, the pinned PDFium bundle and the product
notice exist. It copies the exact verified source archive and packs allowlisted
license, build and PDFium notice bytes into reproducible flat archives. Every
input is hashed into a JSON report; no network, build, vcpkg or native bytes
are created here. The report is evidence, not legal approval.
"""

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import sys
import tarfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

import importlib.util


def load(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


source = load("wfs_bundle", "scripts/windows-ffmpeg-source.py")
overlay = load("wfo_bundle", "scripts/windows-ffmpeg-overlay.py")

SOURCE_VERSION = "8.1.3"
OUTPUTS = (
    "ffmpeg-8.1.3.tar.xz",
    "ffmpeg-legal-notices.tar.gz",
    "ffmpeg-build-materials.tar.gz",
    "pdfium-notices.tar.gz",
    "ffmpeg-source-bundle.json",
)
ARCHIVES = OUTPUTS[1:4]
REPORT_NAME = OUTPUTS[4]
SAFE_REL = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]*(?:/[A-Za-z0-9][A-Za-z0-9_.-]*)*\Z")
SOURCE_TOKEN = "$IRIS_VALIDATION_SOURCE/ffmpeg-8.1.3"
LEGAL_FILES = ("COPYING.LGPLv2.1", "COPYING.GPLv2", "COPYRIGHT", "CREDITS", "LICENSE")
REQUIRED_LEGAL_FILES = ("COPYING.LGPLv2.1", "COPYING.GPLv2")
COPYRIGHT_FILES = ("COPYRIGHT", "CREDITS", "LICENSE")
REPLACEMENT_HEADING = "## Rebuilding and LGPL section 6(b) replacement"
REPLACEMENT_MARKERS = ("section 6(b)", "swap the versioned dll set in place",
                       "no iris object files")
PDFIUM_FILES = ("LICENSE", "VERSION", "args.gn")
VCPKG_FILES = (
    ("vcpkg.json", "packaging/windows/validation/vcpkg.json"),
    ("vcpkg-configuration.json", "packaging/windows/validation/vcpkg-configuration.json"),
    ("triplets/x64-windows-iris-dynamic.cmake",
     "packaging/windows/validation/triplets/x64-windows-iris-dynamic.cmake"),
)
SIGNATURE_FILES = (
    ("signature/ffmpeg-8.1.3.tar.xz.asc", "ffmpeg-8.1.3.tar.xz.asc"),
    ("signature/ffmpeg-key.asc", "ffmpeg-key.asc"),
    ("signature/gpg-status.txt", "status.txt"),
)
MAX_SOURCE_BYTES = 32 * 1024 * 1024
MAX_ARCHIVE_BYTES = 32 * 1024 * 1024
MAX_MEMBER_BYTES = 2 * 1024 * 1024
MAX_REPORT_BYTES = 1024 * 1024
MAX_PATCHES = 64
MAX_PDFIUM_FILES = 64
MAX_PDFIUM_BYTES = 8 * 1024 * 1024


def require(ok, message):
    if not ok:
        raise ValueError(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def binding(data):
    return {"sha256": sha256(data), "bytes": len(data)}


def file_binding(path):
    return binding(read_file(path))


def read_file(path, label=None):
    path = Path(path)
    label = label or str(path)
    require(path.is_file() and not path.is_symlink(), label + " absent or unsupported")
    size = path.stat().st_size
    require(0 < size <= MAX_MEMBER_BYTES, label + " exceeds member bound or is empty")
    return path.read_bytes()


def read_bounded(path, limit, label):
    path = Path(path)
    require(path.is_file() and not path.is_symlink(), label + " absent or unsupported")
    data = path.read_bytes()
    require(0 < len(data) <= limit, label + " exceeds bound or is empty")
    return data


def read_json(path, label):
    value = json.loads(read_file(path, label).decode("utf-8"))
    require(isinstance(value, dict), label + " must be a JSON object")
    return value


def check_rel(name):
    require(isinstance(name, str) and SAFE_REL.fullmatch(name) is not None
            and ".." not in name.split("/"), "unsafe evidence path: " + str(name))
    return name


def pack(path, members, inputs):
    """Write one gzip tar with fixed metadata and record every member hash."""
    check_rel(path.name)
    raw = io.BytesIO()
    records = []
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        for name in sorted(members):
            check_rel(name)
            data = members[name]
            require(isinstance(data, bytes) and 0 < len(data) <= MAX_MEMBER_BYTES,
                    "archive member out of bounds: " + name)
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mtime = 0
            info.mode = 0o644
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            tar.addfile(info, io.BytesIO(data))
            records.append({"path": name, **binding(data)})
    payload = raw.getvalue()
    require(0 < len(payload) <= MAX_ARCHIVE_BYTES, "archive exceeds bound: " + path.name)
    with path.open("wb") as stream:
        with gzip.GzipFile(filename="", mode="wb", fileobj=stream,
                           compresslevel=9, mtime=0) as compressed:
            compressed.write(payload)
    inputs["archive:" + path.name] = {"members": records}
    return file_binding(path)


def replacement_section(notice):
    text = notice.decode("utf-8")
    require(SOURCE_VERSION in text and source.source_pin()["url"] in text,
            "product notice does not bind the verified FFmpeg source")
    require(source.pdfium_pin()["sha256"] in text,
            "product notice does not bind the attested PDFium asset")
    start = text.find(REPLACEMENT_HEADING)
    require(start >= 0, "product notice lacks the DLL replacement section")
    end = text.find("\n## ", start + len(REPLACEMENT_HEADING))
    section = text[start:end if end >= 0 else len(text)].strip() + "\n"
    low = " ".join(section.lower().split())
    for marker in REPLACEMENT_MARKERS:
        require(marker in low, "replacement section lacks marker: " + marker)
    require("release_promotion" not in section and "no iris object files" in low,
            "replacement section must state that dynamic replacement needs no Iris objects")
    return section.encode("utf-8")


def redact(text, source_tree, label):
    variants = {str(source_tree), str(source_tree).replace("\\", "/"),
                json.dumps(str(source_tree))[1:-1]}
    for variant in sorted(variants, key=len, reverse=True):
        text = text.replace(variant, SOURCE_TOKEN)
    require(not re.search(r"[A-Za-z]:[\\/]|\\\\", text),
            label + " still carries a host path after redaction")
    return text.encode("utf-8")


def ffmpeg_members(args, inputs, notice):
    tree = Path(args.source_tree)
    require((tree / "configure").is_file() and (tree / "RELEASE").is_file(),
            "verified source tree layout drifted")
    # windows-ffmpeg-source.py writes the manifest at the staging root,
    # which is the parent of the extracted ffmpeg-<version> directory.
    acquisition = read_json(tree.parent / "acquisition.json", "source acquisition")
    pin = source.source_pin()
    require(acquisition.get("signature_verified") is True
            and acquisition.get("source_sha256") == pin["sha256"]
            and acquisition.get("policy", {}).get("sha256") == pin["sha256"]
            and acquisition.get("policy", {}).get("url") == pin["url"],
            "source acquisition does not bind the signature-verified archive")
    members = {}
    for name in LEGAL_FILES:
        member = "ffmpeg-8.1.3/" + name
        if member not in acquisition.get("files", {}) or (
                name not in REQUIRED_LEGAL_FILES and not (tree / name).is_file()):
            require(name not in REQUIRED_LEGAL_FILES,
                    "verified source lacks required copyright text: " + name)
            continue
        data = read_file(tree / name, "FFmpeg " + name)
        recorded = acquisition["files"][member]
        require(isinstance(recorded, str) and re.fullmatch(r"[0-9a-f]{64}", recorded) is not None,
                "source acquisition digest for " + name + " is unsupported")
        bound = binding(data)
        require(bound["sha256"] == recorded,
                "FFmpeg " + name + " differs from the verified source")
        inputs["ffmpeg-source/" + name] = bound
        members[name] = data
    for name in REQUIRED_LEGAL_FILES:
        require(name in members, "missing required FFmpeg legal text: " + name)
    require(any(name in members for name in COPYRIGHT_FILES),
            "verified source lacks upstream copyright/credits text")
    members["FFMPEG-NOTICE.md"] = notice
    members["REPLACEMENT.md"] = replacement_section(notice)
    inputs["ffmpeg-notice"] = binding(notice)
    return members, acquisition


def overlay_members(args, inputs, acquisition, section):
    port = Path(args.overlay) / "ffmpeg"
    require(port.is_dir() and not port.is_symlink(), "derived vcpkg overlay port missing")
    top = sorted(entry.name for entry in port.iterdir())
    allowed = {"portfile.cmake", "portfile.evidence.json", "vcpkg.json",
               "usage", "patches"}
    require(set(top) <= allowed and "portfile.cmake" in top
            and "portfile.evidence.json" in top and "vcpkg.json" in top,
            "derived overlay port has an unexpected file set: " + repr(top))
    members = {"BUILD-INSTRUCTIONS.md": section,
               "source-acquisition.json": json.dumps(acquisition, indent=2,
                                                     sort_keys=True).encode("utf-8") + b"\n"}
    inputs["source-acquisition"] = binding(members["source-acquisition.json"])
    for name, relative in VCPKG_FILES:
        data = read_file(ROOT / relative, "vcpkg " + name)
        members[name] = data
        inputs["vcpkg/" + name] = binding(data)
    for name in ("portfile.cmake", "portfile.evidence.json", "vcpkg.json", "usage"):
        if name not in top:
            continue
        data = read_file(port / name, "overlay " + name)
        inputs["overlay/ffmpeg/" + name] = binding(data)
        members["overlay/ffmpeg/" + name] = redact(
            data.decode("utf-8"), Path(args.source_tree), "overlay " + name)
    patches = port / "patches"
    require(patches.is_dir() and not patches.is_symlink(), "overlay patches directory missing")
    names = sorted(entry.name for entry in patches.iterdir())
    require(0 < len(names) <= MAX_PATCHES, "overlay patch count out of bounds")
    require(set(overlay.baseline_patches()) <= set(names),
            "derived overlay lost a pinned baseline patch")
    for name in names:
        check_rel("overlay/ffmpeg/patches/" + name)
        require(name.endswith(".patch"), "unexpected non-patch overlay file: " + name)
        data = read_file(patches / name, "overlay patch " + name)
        inputs["overlay/ffmpeg/patches/" + name] = binding(data)
        members["overlay/ffmpeg/patches/" + name] = data
    gpg = Path(args.gpg_dir)
    for archive_name, basename in SIGNATURE_FILES:
        data = read_file(gpg / basename, "signature " + basename)
        inputs["signature/" + basename] = binding(data)
        members[archive_name] = data
    return members


def pdfium_members(args, inputs):
    pin = source.pdfium_pin()
    asset = Path(args.pdfium_asset)
    require(asset.is_file() and not asset.is_symlink(), "attested PDFium asset missing")
    raw = read_bounded(asset, MAX_ARCHIVE_BYTES, "PDFium asset")
    require(len(raw) == pin["bytes"] and sha256(raw) == pin["sha256"],
            "PDFium asset differs from the attested pin")
    inputs["pdfium-asset"] = binding(raw)
    root = Path(args.pdfium_extract)
    require(root.is_dir() and not root.is_symlink(), "PDFium extract missing")
    members = {}
    for name in PDFIUM_FILES:
        data = read_file(root / name, "PDFium " + name)
        members[name] = data
        inputs["pdfium/" + name] = binding(data)
    version = members["VERSION"].decode("utf-8")
    require("MAJOR=159" in version and "BUILD=8138" in version,
            "PDFium VERSION does not match the approved release")
    licenses = root / "licenses"
    require(licenses.is_dir() and not licenses.is_symlink(), "PDFium licenses missing")
    total, paths = 0, []
    pending = [licenses]
    while pending:
        directory = pending.pop()
        require(not directory.is_symlink(), "symlinked PDFium license directory")
        for entry in sorted(directory.iterdir(), key=lambda item: item.name):
            require(not entry.is_symlink(), "symlinked PDFium license file")
            rel = "licenses/" + entry.relative_to(licenses).as_posix()
            check_rel(rel)
            if entry.is_dir():
                pending.append(entry)
                continue
            require(entry.is_file() and len(paths) < MAX_PDFIUM_FILES,
                    "unsupported/oversized PDFium license entry: " + rel)
            data = read_file(entry, "PDFium " + rel)
            total += len(data)
            require(total <= MAX_PDFIUM_BYTES, "PDFium notices exceed byte bound")
            members[rel] = data
            inputs["pdfium/" + rel] = binding(data)
            paths.append(rel)
    require("licenses/pdfium.txt" in paths, "PDFium third-party notice missing")
    return members


def build(args):
    pin = source.source_pin()
    require(pin["version"] == SOURCE_VERSION, "unapproved FFmpeg version")
    archive = Path(args.source_archive)
    require(archive.is_file() and not archive.is_symlink(), "verified source archive missing")
    data = read_bounded(archive, MAX_SOURCE_BYTES, "FFmpeg source archive")
    require(len(data) == archive.stat().st_size and sha256(data) == pin["sha256"],
            "source archive differs from the signature-verified pin")
    inputs = {"ffmpeg-source-archive": binding(data)}
    notice = read_file(args.notice, "product notice")
    legal, acquisition = ffmpeg_members(args, inputs, notice)
    section = legal["REPLACEMENT.md"]
    inputs["ffmpeg-source/acquisition.json"] = file_binding(
        Path(args.source_tree).parent / "acquisition.json")
    build_materials = overlay_members(args, inputs, acquisition, section)
    pdfium = pdfium_members(args, inputs)

    out = Path(args.output)
    require(out.is_dir() and not out.is_symlink(), "product stage missing")
    for name in OUTPUTS:
        target = out / name
        require(not target.is_symlink(), "unsafe existing product stage entry: " + name)
        if target.exists():
            target.unlink()
    (out / OUTPUTS[0]).write_bytes(data)
    outputs = {OUTPUTS[0]: binding(data)}
    for name, members in ((ARCHIVES[0], legal), (ARCHIVES[1], build_materials),
                          (ARCHIVES[2], pdfium)):
        outputs[name] = pack(out / name, members, inputs)
    report = {"schema": 1, "ffmpeg_version": SOURCE_VERSION,
              "pdfium_release": source.pdfium_pin()["release"],
              "inputs": inputs, "outputs": outputs}
    text = (json.dumps(report, indent=2, sort_keys=True) + "\n").encode("utf-8")
    require(len(text) <= MAX_REPORT_BYTES, "source bundle report exceeds bound")
    (out / REPORT_NAME).write_bytes(text)
    if args.report:
        report_path = Path(args.report)
        report_path.parent.mkdir(parents=True, exist_ok=True)
        require(not report_path.is_symlink(), "unsafe report path")
        report_path.write_bytes(text)
    print(json.dumps({"status": "bundled", "outputs": {
        name: outputs[name] for name in OUTPUTS[:4]}}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("plan")
    build_parser = commands.add_parser("build")
    for flag in ("source-archive", "source-tree", "gpg-dir", "overlay",
                 "pdfium-asset", "pdfium-extract", "notice", "output"):
        build_parser.add_argument("--" + flag, required=True, type=Path)
    build_parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "plan":
            print(json.dumps({"version": SOURCE_VERSION, "outputs": list(OUTPUTS),
                              "archives": list(ARCHIVES), "report": REPORT_NAME,
                              "legal_files": list(REQUIRED_LEGAL_FILES + COPYRIGHT_FILES),
                              "pdfium_files": list(PDFIUM_FILES) + ["licenses/pdfium.txt"],
                              "source_token": SOURCE_TOKEN,
                              "bounds": {"source_bytes": MAX_SOURCE_BYTES,
                                         "archive_bytes": MAX_ARCHIVE_BYTES,
                                         "member_bytes": MAX_MEMBER_BYTES,
                                         "patches": MAX_PATCHES,
                                         "pdfium_files": MAX_PDFIUM_FILES,
                                         "pdfium_bytes": MAX_PDFIUM_BYTES}}, indent=2))
            return 0
        build(args)
        return 0
    except (OSError, ValueError, TypeError, KeyError, UnicodeError) as error:
        print("Windows source bundle refused: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
