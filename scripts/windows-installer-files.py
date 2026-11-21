#!/usr/bin/env python3
"""Render Inno Setup [Files] entries from a bounded product inventory.

The inventory is data from an exact-SHA stage; nothing here builds, downloads
or publishes anything, and importing this module has no side effects.
"""

import json
import os
from pathlib import Path
import re
import sys

MAX_ENTRIES = 256
MAX_NAME_LENGTH = 128
MAX_FILE_BYTES = 1 << 30
MAX_INVENTORY_BYTES = 1 << 20
MAX_OUTPUT_BYTES = 1 << 16
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
LINE_PREFIX = 'Source: "{#SourceDir}\\'
LINE_SUFFIX = '"; DestDir: "{app}"; Flags: ignoreversion'


def _valid_name(name):
    # Inno lines are quoted, one per line and semicolon-delimited: any
    # separator, quote, control character or non-ASCII byte could change them.
    return (
        isinstance(name, str)
        and 0 < len(name) <= MAX_NAME_LENGTH
        and name not in (".", "..")
        and "/" not in name
        and "\\" not in name
        and name == name.strip()
        and not any(
            ord(char) < 0x20 or ord(char) > 0x7E or char in "\"';"
            for char in name
        )
    )


def installer_files(inventory_path):
    """Return one [Files] line per inventory entry, in inventory order."""
    inventory_file = Path(inventory_path)
    if inventory_file.stat().st_size > MAX_INVENTORY_BYTES:
        raise ValueError("product inventory too large")
    try:
        inventory = json.loads(inventory_file.read_text(encoding="utf-8"))
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise ValueError("invalid product inventory JSON") from exc
    if not isinstance(inventory, dict) or not isinstance(inventory.get("files"), list):
        raise ValueError("product inventory has no files")
    files = inventory["files"]
    if not 1 <= len(files) <= MAX_ENTRIES:
        raise ValueError("product inventory entry count out of bounds")

    seen = set()
    lines = []
    for item in files:
        if not isinstance(item, dict):
            raise ValueError("invalid product inventory entry")
        name = item.get("name")
        if not _valid_name(name):
            raise ValueError(f"unsafe product inventory name: {name!r}")
        key = name.casefold()
        if key in seen:
            raise ValueError(f"duplicate product inventory name: {name}")
        seen.add(key)
        sha256 = item.get("sha256")
        if not isinstance(sha256, str) or not SHA256_RE.fullmatch(sha256):
            raise ValueError(f"invalid product inventory sha256: {name}")
        size = item.get("bytes")
        if (isinstance(size, bool) or not isinstance(size, int)
                or not 0 < size <= MAX_FILE_BYTES):
            raise ValueError(f"invalid product inventory byte count: {name}")
        lines.append(LINE_PREFIX + name + LINE_SUFFIX)
    return lines


def main(argv=None):
    args = sys.argv[1:] if argv is None else list(argv)
    if len(args) != 2:
        print("usage: windows-installer-files.py <inventory> <output>", file=sys.stderr)
        return 2
    output = Path(args[1])
    stage = os.environ.get("IRIS_PRODUCT_STAGE")
    if stage:
        stage_path = Path(stage).resolve()
        output_path = output.resolve()
        if output_path == stage_path or stage_path in output_path.parents:
            print("installer file list must be outside the product stage", file=sys.stderr)
            return 1
    if not output.parent.is_dir():
        print("installer file list parent directory missing", file=sys.stderr)
        return 1
    text = "\n".join(installer_files(args[0])) + "\n"
    if len(text.encode("utf-8")) > MAX_OUTPUT_BYTES:
        raise ValueError("installer file list exceeds bound")
    output.write_text(text, encoding="utf-8", newline="\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
