#!/usr/bin/env python3
"""Check that final POSIX release payloads carry the exact pinned notice bytes.

The pins live in scripts/linux-ffmpeg.py::RUST_NOTICES, which also gates notice
staging. This verifier reuses that list verbatim: it accepts no alternate pin
source. It reads only a directory tree, a .tar.gz or a .zip; a macOS PKG is
verified after the release workflow expands its payload with pkgutil.
"""
import argparse
from importlib.util import module_from_spec, spec_from_file_location
import hashlib
import os
from pathlib import Path
import sys
import tarfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True

# Suffix under each platform's payload root. Any Iris.AppDir / Iris.app /
# expanded PKG Payload prefix above it is deliberately irrelevant.
DIR_SUFFIX = {
    "linux": "usr/share/doc/iris/ffmpeg",
    "macos": "Contents/Resources/ffmpeg",
}
CHUNK = 1 << 20


def require(ok, message):
    if not ok:
        raise ValueError(message)


def pinned_notices():
    """Import the single reviewed pin table; never restate pins in this file."""
    spec = spec_from_file_location("iris_ffmpeg_notice_pins", ROOT / "scripts/linux-ffmpeg.py")
    require(spec is not None and spec.loader is not None, "notice pin source unavailable")
    module = module_from_spec(spec)
    spec.loader.exec_module(module)
    pins = module.RUST_NOTICES
    require(bool(pins) and all(len(row) == 3 for row in pins), "invalid notice pin table")
    return pins


def sha256_stream(stream):
    digest = hashlib.sha256()
    for chunk in iter(lambda: stream.read(CHUNK), b""):
        digest.update(chunk)
    return digest.hexdigest()


def member_name(name):
    """Normalize an archive member to a safe payload-relative POSIX path."""
    require(isinstance(name, str) and name and "\x00" not in name, "invalid archive member")
    parts = name.lstrip("/").split("/")
    while parts and parts[0] == ".":
        parts.pop(0)
    require(parts and all(part not in ("", "..") for part in parts),
            "unsafe archive member: " + str(name))
    return "/".join(parts)


def matched_notice(relative, platform, names):
    """Return the pinned notice name when a path ends in this platform's docs dir."""
    directory = DIR_SUFFIX[platform]
    if relative.startswith(directory + "/"):
        tail = relative[len(directory) + 1:]
    else:
        marker = "/" + directory + "/"
        position = relative.find(marker)
        if position < 0:
            return None
        tail = relative[position + len(marker):]
    return tail if "/" not in tail and tail in names else None


def record(found, relative, platform, names, stream):
    name = matched_notice(relative, platform, names)
    if name is not None:
        found[name].append((relative, sha256_stream(stream)))


def scan_directory(payload, platform, names):
    require(payload.is_dir() and not payload.is_symlink(),
            "payload root is not a directory: " + str(payload))
    found = {name: [] for name in names}
    root = payload.resolve()
    for base, dirs, files in os.walk(root):
        dirs[:] = sorted(name for name in dirs if not (Path(base) / name).is_symlink())
        for filename in sorted(files):
            if filename not in names:
                continue
            path = Path(base) / filename
            # A symlink is not notice bytes shipped in the payload.
            if path.is_symlink() or not path.is_file():
                continue
            relative = path.relative_to(root).as_posix()
            with path.open("rb") as stream:
                record(found, relative, platform, names, stream)
    return found


def scan_tar(payload, platform, names):
    found = {name: [] for name in names}
    with tarfile.open(payload, "r:gz") as archive:
        for member in archive:
            if not member.isfile():
                continue
            relative = member_name(member.name)
            if relative.rsplit("/", 1)[-1] not in names:
                continue
            stream = archive.extractfile(member)
            require(stream is not None, "unreadable archive member: " + relative)
            with stream:
                record(found, relative, platform, names, stream)
    return found


def scan_zip(payload, platform, names):
    found = {name: [] for name in names}
    with zipfile.ZipFile(payload) as archive:
        for info in archive.infolist():
            mode = (info.external_attr >> 16) & 0o170000
            if info.is_dir() or mode == 0o120000:  # directory or symlink
                continue
            relative = member_name(info.filename)
            if relative.rsplit("/", 1)[-1] not in names:
                continue
            with archive.open(info) as stream:
                record(found, relative, platform, names, stream)
    return found


def verify(payload, platform):
    """Fail unless every pinned notice has the exact pinned bytes in payload."""
    pins = pinned_notices()
    names = {name for name, _, _ in pins}
    if payload.is_dir():
        found = scan_directory(payload, platform, names)
    elif payload.name.endswith((".tar.gz", ".tgz")):
        found = scan_tar(payload, platform, names)
    elif payload.name.endswith(".zip"):
        found = scan_zip(payload, platform, names)
    else:
        raise ValueError("unsupported payload (expected directory, tar.gz or zip): " + str(payload))
    for name, _, expected in pins:
        matches = found.get(name, [])
        require(matches, platform + " payload missing pinned notice: " + name)
        for relative, actual in matches:
            require(actual == expected,
                    platform + " payload notice changed: " + name + " at " + relative)
    return len(pins)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("payload", type=Path, help="payload tree, tar.gz or zip")
    parser.add_argument("--platform", choices=sorted(DIR_SUFFIX), required=True)
    args = parser.parse_args(argv)
    try:
        count = verify(args.payload, args.platform)
    except (ValueError, OSError, tarfile.TarError, zipfile.BadZipFile) as error:
        print("notice verification failed: " + str(error), file=sys.stderr)
        return 1
    print(f"verified {count} pinned notices in {args.platform} payload")
    return 0


if __name__ == "__main__":
    sys.exit(main())
