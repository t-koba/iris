#!/usr/bin/env python3
"""Derive a production Windows vcpkg overlay that uses the verified local FFmpeg source.

The pinned vcpkg baseline (b8dfe957) ffmpeg port fetches FFmpeg 9.0.2 from
GitHub with a PATCHES list written for that release. The production build must
instead use the signature-verified 8.1.3 source tree already staged by
scripts/windows-ffmpeg-source.py. This module performs a pure, offline source
rewrite: it validates the exact known baseline fetch shape and replaces only
that fetch with a direct SOURCE_PATH assignment to the verified local tree.

Fail-closed guarantees:
- the baseline fetch must match the single known shape (digest recorded);
- no git/unsigned source, URL fetch, or version fallback may appear in output;
- the verified source path must be absolute and free of spaces (a vcpkg
  constraint stated in the baseline portfile) and shell metacharacters;
- baseline PATCHES are not applied: they target 9.0.2 and are unvalidated for
  8.1.3. Any required patch must be justified from exact pinned-source context
  in a native build validated by exact-SHA Windows CI.

No download, vcpkg invocation, patch application or native build occurs here.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]

# The exact pinned baseline fetch block (receipt 8179a47c, portfile sha256
# 572a948b67e35345120a045c925344f1c06cb3e1ea7c3dee86bc1375656ccd2c). Keeping
# it here makes the rewrite shape check independent of a vcpkg checkout.
BASELINE_FETCH = '''vcpkg_from_github(
    OUT_SOURCE_PATH SOURCE_PATH
    REPO ffmpeg/ffmpeg
    REF "n${VERSION}"
    SHA512 21bf3fbcdfd2f41ea6edeab40433fecfe362b09ecaa177a652471b54f9357011b4f74dab18245ab1c26f1a473b94205490baecb42289afd2effd603cd0b22b59
    HEAD_REF master
    PATCHES
        0003-fix-windowsinclude.patch
        0004-dependencies.patch
        0005-fix-nasm.patch
        0007-fix-lib-naming.patch
        0013-define-WINVER.patch
        0024-fix-osx-host-c11.patch
        0040-ffmpeg-add-av_stream_get_first_dts-for-chromium.patch # Do not remove this patch. It is required by chromium
        0045-use-prebuilt-bin2c.patch
        0046-fix-msvc-detection.patch
        0047-fix-msvc-utf8.patch
        0049-fix-twolame-pkgconfig.patch
        0050-fix-test-ld-absolute-lib-paths.patch
        0051-fix-msvc-undef-flags.patch
        0052-fix-disable-unstable-swscale-link.patch
)
'''

FETCH_RE = re.compile(r"^vcpkg_from_github\(\s*\n.*?^\)[ \t]*\r?\n", re.M | re.S)
# Source acquisition must be entirely local in the production overlay.
SOURCE_UNSAFE = ("vcpkg_from_git", "vcpkg_from_github", "vcpkg_download_distfile",
                 "git+", "db8537d8", "n${VERSION}")
# The baseline gates these behind vcpkg features; they are inert unless the
# caller selects them, so they are controlled by validate_features(), not by
# scanning the (pinned) portfile text.
FORBIDDEN_FEATURES = ("gpl", "version3", "nonfree")
REPLACEMENT_UNSAFE = ("--enable-gpl", "--enable-version3", "--enable-nonfree")


def require(ok, message):
    if not ok:
        raise ValueError(message)


def baseline_patches():
    """Return the ordered baseline patch names from the validated fetch block."""
    lines = BASELINE_FETCH.splitlines()
    start = lines.index("    PATCHES") + 1
    patches = []
    for line in lines[start:]:
        name = line.split("#", 1)[0].strip()
        if name and name != ")":
            patches.append(name)
    return patches


def validate_source_dir(path):
    """A vcpkg SOURCE_PATH must be absolute, space-free and shell-safe."""
    text = str(path)
    require(Path(text).is_absolute(), "verified source path must be absolute")
    require(" " not in text and "\t" not in text, "vcpkg cannot build with spaces in the source path")
    require(not re.search(r"[;&|`$\"']", text), "unsafe characters in verified source path")
    return Path(text)


def validate_features(features):
    """Reject vcpkg feature selections that would make Iris GPL/AGPL-encumbered."""
    for feature in FORBIDDEN_FEATURES:
        require(feature not in features,
                f"forbidden vcpkg feature {feature!r} would require a GPL/AGPL release")
    return list(features)


def derive(portfile, source_dir):
    """Rewrite a pinned baseline portfile to use the verified local source tree.

    Returns (rewritten_portfile, evidence). The caller supplies the portfile
    text from the pinned vcpkg checkout; this function never reads it from an
    unpinned location.
    """
    digest = hashlib.sha256(BASELINE_FETCH.encode()).hexdigest()
    require(baseline_patches(), "baseline fetch has no patches")
    matches = FETCH_RE.findall(portfile)
    require(len(matches) == 1, "expected exactly one baseline source fetch")
    fetch = matches[0]
    require(fetch == BASELINE_FETCH,
            "baseline fetch shape drifted from the validated pinned block")
    require("REPO ffmpeg/ffmpeg" in fetch and 'REF "n${VERSION}"' in fetch,
            "unexpected baseline fetch identity")

    source = validate_source_dir(source_dir)
    replacement = (
        "# Production overlay: use the signature-verified FFmpeg 8.1.3 source\n"
        "# staged by windows-ffmpeg-source.py. No network fetch or git source.\n"
        'set(SOURCE_PATH "%s")\n' % source
    )
    for marker in REPLACEMENT_UNSAFE:
        require(marker not in replacement,
                f"unsafe licence flag {marker} in generated overlay source block")
    rewritten = portfile.replace(fetch, replacement, 1)
    for marker in SOURCE_UNSAFE:
        require(marker not in rewritten,
                f"unsafe source marker {marker} would remain in the overlay")
    require(rewritten.count("set(SOURCE_PATH ") == 1, "overlay must set exactly one SOURCE_PATH")

    evidence = dict(
        baseline_fetch_sha256=hashlib.sha256(fetch.encode()).hexdigest(),
        baseline_portfile_sha256=hashlib.sha256(portfile.encode()).hexdigest(),
        baseline_ffmpeg_version="9.0.2",
        production_ffmpeg_version="8.1.3",
        source_path=str(source),
        source_path_sha256=digest,
        baseline_patches_not_applied=baseline_patches(),
        forbidden_features=list(FORBIDDEN_FEATURES),
        patch_note="Baseline patches target FFmpeg 9.0.2 and are unvalidated for 8.1.3; "
                   "none are applied. Any required patch needs exact pinned-source context "
                   "and native validation via exact-SHA Windows CI.",
    )
    return rewritten, evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["patches", "derive"])
    parser.add_argument("portfile", nargs="?", type=Path)
    parser.add_argument("source_dir", nargs="?")
    parser.add_argument("output", nargs="?", type=Path)
    args = parser.parse_args()
    if args.command == "patches":
        print(json.dumps(baseline_patches(), indent=2))
        return
    require(args.portfile and args.source_dir and args.output,
            "derive expects PORTFILE SOURCE_DIR OUTPUT")
    rewritten, evidence = derive(args.portfile.read_text(), args.source_dir)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(rewritten)
    args.output.with_suffix(".evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print("derived")


if __name__ == "__main__":
    main()
