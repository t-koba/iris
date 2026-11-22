#!/usr/bin/env python3
"""Verify the installed Windows payload against the exact release inventory.

The release workflow installs the exported Iris-windows-setup.exe into a clean
directory. This read-only checker compares every installed file with the
exported product inventory (name, size, SHA-256), and treats the Inno Setup
uninstaller files as a separate, bounded and recorded class. The setup binary
is re-bound to the installer hash in the exported release result before any
payload comparison happens. No build, download or publication is performed.
"""

import argparse
import hashlib
import json
import os
import re
import sys
from pathlib import Path

MAX_INVENTORY_BYTES = 1 << 20
MAX_RESULT_BYTES = 1 << 16
MAX_ENTRIES = 256
MAX_NAME_LENGTH = 128
MAX_FILE_BYTES = 1 << 30
MAX_SETUP_BYTES = 1 << 29
MAX_INSTALLER_OWNED_ENTRIES = 3
MAX_INSTALLER_OWNED_BYTES = 1 << 25
# Inno Setup writes these itself into {app}; they are never product files.
# A future helper must be added here with a reason and a test, not skipped by
# a wildcard or an "ignore the rest" rule.
INSTALLER_OWNED = frozenset({"unins000.exe", "unins000.dat", "unins000.msg"})
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
CHUNK_BYTES = 1 << 20
SETUP_NAME = "iris-windows-setup.exe"


def _read_json(path, label, limit):
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"{label} missing or not a regular file")
    raw = path.read_bytes()
    if not raw or len(raw) > limit:
        raise ValueError(f"{label} size out of bounds")
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError(f"{label} is not valid UTF-8 JSON") from exc
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value, hashlib.sha256(raw).hexdigest()


def _valid_name(name):
    return (
        isinstance(name, str)
        and 0 < len(name) <= MAX_NAME_LENGTH
        and name not in (".", "..")
        and name == name.strip()
        and "/" not in name
        and "\\" not in name
        and all(0x20 <= ord(char) <= 0x7E and char not in "\"';" for char in name)
    )


def _hash_file(path, label, limit):
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"{label} missing or not a regular file")
    size = path.stat().st_size
    if size <= 0 or size > limit:
        raise ValueError(f"{label} size out of bounds")
    digest = hashlib.sha256()
    remaining = size
    with path.open("rb") as handle:
        while remaining:
            chunk = handle.read(min(CHUNK_BYTES, remaining))
            if not chunk:
                raise ValueError(f"{label} truncated while hashing")
            digest.update(chunk)
            remaining -= len(chunk)
        if handle.read(1):
            raise ValueError(f"{label} grew while hashing")
    return digest.hexdigest(), size


def _load_inventory(path):
    data, digest = _read_json(path, "product inventory", MAX_INVENTORY_BYTES)
    files = data.get("files")
    if not isinstance(files, list) or not files or len(files) > MAX_ENTRIES:
        raise ValueError("product inventory entry count out of bounds")
    expected = {}
    for item in files:
        if not isinstance(item, dict):
            raise ValueError("product inventory entry is not an object")
        name = item.get("name")
        if not _valid_name(name):
            raise ValueError("unsafe product inventory name")
        key = name.lower()
        if key in INSTALLER_OWNED:
            raise ValueError(f"installer-owned file in product inventory: {name}")
        sha256 = item.get("sha256")
        size = item.get("bytes")
        if not isinstance(sha256, str) or not SHA256_RE.fullmatch(sha256):
            raise ValueError(f"invalid product inventory sha256: {name}")
        if isinstance(size, bool) or not isinstance(size, int) or not 0 < size <= MAX_FILE_BYTES:
            raise ValueError(f"invalid product inventory byte count: {name}")
        if key in expected:
            raise ValueError(f"duplicate product inventory name: {name}")
        expected[key] = (name, sha256, size)
    if "iris.exe" not in expected:
        raise ValueError("Iris.exe missing from product inventory")
    return expected, digest


def _load_result(path, expected):
    data, _ = _read_json(path, "release result", MAX_RESULT_BYTES)
    if data.get("status") != "pass":
        raise ValueError("release result is not pass")
    sha256 = data.get("installer_sha256")
    if not isinstance(sha256, str) or not SHA256_RE.fullmatch(sha256):
        raise ValueError("invalid exported installer sha256")
    size = data.get("installer_bytes")
    if isinstance(size, bool) or not isinstance(size, int) or not 0 < size <= MAX_SETUP_BYTES:
        raise ValueError("invalid exported installer byte count")
    entries = data.get("entries")
    if not isinstance(entries, list) or len(entries) != len(expected):
        raise ValueError("release result entry count differs from inventory")
    seen = set()
    for name in entries:
        if not _valid_name(name) or name.lower() in seen:
            raise ValueError("invalid release result entry")
        seen.add(name.lower())
    if seen != set(expected):
        raise ValueError("release result entries differ from inventory")
    return sha256, size


def verify_install(install_dir, inventory_path, result_path, setup_path):
    """Return a bound pass record or raise ValueError on any failed gate."""
    expected, inventory_digest = _load_inventory(inventory_path)
    expected_sha256, expected_bytes = _load_result(result_path, expected)
    setup_path = Path(setup_path)
    if setup_path.name.lower() != SETUP_NAME:
        raise ValueError("setup file is not Iris-windows-setup.exe")
    setup_sha256, setup_bytes = _hash_file(setup_path, "setup", MAX_SETUP_BYTES)
    if setup_sha256 != expected_sha256 or setup_bytes != expected_bytes:
        raise ValueError("Iris-windows-setup.exe differs from exported installer")

    install_dir = Path(install_dir)
    if install_dir.is_symlink() or not install_dir.is_dir():
        raise ValueError("clean install directory missing or not a directory")
    seen = set()
    installer_owned = []
    matched = 0
    with os.scandir(install_dir) as entries:
        for entry in entries:
            name = entry.name
            if not _valid_name(name):
                raise ValueError(f"unsafe installed entry: {name[:MAX_NAME_LENGTH]}")
            key = name.lower()
            if key in seen:
                raise ValueError(f"duplicate installed entry: {name}")
            seen.add(key)
            if entry.is_dir(follow_symlinks=False) or not entry.is_file(follow_symlinks=False):
                raise ValueError(f"unexpected installed directory or link: {name}")
            if key in INSTALLER_OWNED:
                digest, size = _hash_file(entry.path, f"installer-owned {name}", MAX_INSTALLER_OWNED_BYTES)
                installer_owned.append({"name": name, "bytes": size, "sha256": digest})
                if len(installer_owned) > MAX_INSTALLER_OWNED_ENTRIES:
                    raise ValueError("too many installer-owned entries")
                continue
            item = expected.get(key)
            if item is None:
                raise ValueError(f"unexpected installed file: {name}")
            want_name, want_sha256, want_bytes = item
            if name != want_name:
                raise ValueError(f"installed name differs in case: {name}")
            digest, size = _hash_file(entry.path, f"product file {name}", MAX_FILE_BYTES)
            if size != want_bytes or digest != want_sha256:
                raise ValueError(f"installed product file differs: {name}")
            matched += 1
    missing = sorted(want_name for key, (want_name, _, _) in expected.items() if key not in seen)
    if missing:
        raise ValueError("missing installed product files: " + ",".join(missing[:8]))
    return {
        "status": "pass",
        "installer_sha256": setup_sha256,
        "inventory_sha256": inventory_digest,
        "product_entries": len(expected),
        "matched_entries": matched,
        "installer_owned": sorted(installer_owned, key=lambda row: row["name"]),
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("verify", help="compare installed files with the exported inventory")
    check.add_argument("--install-dir", required=True, type=Path)
    check.add_argument("--inventory", required=True, type=Path)
    check.add_argument("--result", required=True, type=Path)
    check.add_argument("--setup", required=True, type=Path)
    check.add_argument("--output", type=Path)
    args = parser.parse_args(argv)
    try:
        record = verify_install(args.install_dir, args.inventory, args.result, args.setup)
    except (OSError, ValueError) as exc:
        message = str(exc)[:512]
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps({"status": "fail", "error": message}, indent=2) + "\n",
                                   encoding="utf-8")
        print("installed payload verification failed: " + message, file=sys.stderr)
        return 1
    text = json.dumps(record, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text, encoding="utf-8")
    print(text, end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
