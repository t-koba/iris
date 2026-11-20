#!/usr/bin/env python3
"""Signature-verified source acquisition for the approved production Windows FFmpeg runtime.

This is the build input for the plan in packaging/windows/ffmpeg.json. It sources
the same signature-verified FFmpeg 8.1.3 tarball approved for Linux and macOS
(packaging/linux/ffmpeg.json) plus the pinned Windows PDFium asset. It never uses
a test-only prototype's unsigned source or binding override.

This module performs acquisition and verification only. vcpkg overlay derivation,
native build, DLL/PDFium closure staging, notices and clean-install preview smoke
remain for a Windows runner via exact-SHA CI; release.yml stays minimal until then.
The GPG signature verifier and digest helper are imported from linux-ffmpeg.py so
there is a single approved verification implementation.
"""

import argparse
import importlib.util
import json
from pathlib import Path
import re
import sys
import tarfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]

# Reuse the single approved signature verifier; do not duplicate verification logic.
_spec = importlib.util.spec_from_file_location("ffmpeg_policy", ROOT / "scripts/linux-ffmpeg.py")
common = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(common)
require, signature, digest = common.require, common.signature, common.digest

PLAN = json.loads((ROOT / "packaging/windows/ffmpeg.json").read_text())


def source_pin():
    """Resolve and validate the approved FFmpeg source pin referenced by the plan."""
    ref = PLAN["ffmpeg"]["source"]
    require(ref == "packaging/linux/ffmpeg.json", "FFmpeg source must reference the approved pin")
    pin = json.loads((ROOT / ref).read_text())
    require(pin["version"] == "8.1.3", "unapproved FFmpeg version")
    require(pin["avcodec_version"] == [62, 28, 103], "unapproved FFmpeg ABI")
    require(pin["url"] == "https://ffmpeg.org/releases/ffmpeg-8.1.3.tar.xz",
            "unapproved FFmpeg source URL")
    for field in ("sha256", "signature_url", "key_url", "fingerprint"):
        require(isinstance(pin.get(field), str) and pin[field], f"missing source field {field}")
    require(pin["fingerprint"] == common.POLICY["fingerprint"],
            "fingerprint differs from the shared verifier")
    require(PLAN["bindings"]["ffmpeg-sys-next"] == "9.0.1", "unapproved binding")
    require(PLAN["bindings"]["ffmpeg-next"] == "9.0.0", "unapproved binding")
    return pin


def linux_attestation_pin():
    """Read the authoritative attestation pin from the Linux staging contract."""
    text = (ROOT / "scripts/fetch-pdfium-runtime.sh").read_text()

    def shell_pin(name):
        match = re.search(rf'^{re.escape(name)}=(?:"([^"]+)"|([^\s#]+))$',
                          text, re.MULTILINE)
        require(match is not None, f"missing Linux PDFium pin {name}")
        return match.group(1) or match.group(2)

    bundle_bytes = shell_pin("pdfium_attestation_bytes")
    require(bundle_bytes.isdigit(), "malformed Linux attestation size pin")
    commit = shell_pin("pdfium_commit")
    return dict(
        attestation_asset="pdfium-attestation.json",
        attestation_bytes=int(bundle_bytes),
        attestation_sha256=shell_pin("pdfium_attestation_sha256"),
        repo=shell_pin("pdfium_repo"),
        signer_workflow=shell_pin("pdfium_signer_workflow"),
        signer_digest=commit,
        source_digest=commit,
        source_ref=shell_pin("pdfium_source_ref"),
    )


def pdfium_pin():
    """Resolve and validate the pinned Windows PDFium asset from the plan."""
    pdfium = PLAN["pdfium"]
    require(pdfium["release"] == "chromium/8138", "unapproved PDFium release")
    require(pdfium["version"] == "159.0.8138.0", "unapproved PDFium version")
    require(pdfium["asset"] == "pdfium-win-x64.tgz", "unapproved PDFium asset")
    require(pdfium["url"] ==
            "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8138/pdfium-win-x64.tgz",
            "unapproved PDFium URL")
    require(len(pdfium["sha256"]) == 64 and all(c in "0123456789abcdef" for c in pdfium["sha256"]),
            "malformed PDFium sha256")
    require(isinstance(pdfium["bytes"], int) and pdfium["bytes"] > 0, "malformed PDFium size")
    require(pdfium["attestation_required"] is True, "PDFium attestation must be required")
    # The bundle pin is shared with the Linux contract; emit it only when it is
    # identical, so a Windows-only drift cannot produce a truthful-looking plan.
    for field, expected in linux_attestation_pin().items():
        require(pdfium.get(field) == expected,
                f"PDFium attestation {field} differs from the Linux contract")
    return pdfium


def verify_archive(archive, status_text, pin=None):
    """Require both the pinned sha256 and a valid signature from the pinned key."""
    pin = pin if pin is not None else source_pin()
    require(digest(archive) == pin["sha256"], "source sha256 mismatch")
    signature(status_text)
    return True


def _safe_members(archive, destination):
    """Yield (member, target) pairs, rejecting absolute paths and traversal."""
    root = destination.resolve()
    for member in archive.getmembers():
        if not (member.isfile() or member.isdir()):
            raise ValueError(f"unsupported archive member type: {member.name}")
        target = (destination / member.name).resolve()
        if target != root and root not in target.parents:
            raise ValueError(f"unsafe archive member path: {member.name}")
        yield member, target


def stage(archive, status_text, destination, pin=None):
    """Verify, extract safely, and write an evidence manifest; fail closed on drift."""
    pin = pin if pin is not None else source_pin()
    verify_archive(archive, status_text, pin)
    destination.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archive, "r:xz") as tar:
        for member, target in _safe_members(tar, destination):
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                source = tar.extractfile(member)
                require(source is not None, f"unreadable archive member: {member.name}")
                with source, target.open("wb") as output:
                    while True:
                        block = source.read(1024 * 1024)
                        if not block:
                            break
                        output.write(block)
    manifest = dict(
        policy=pin,
        signature_verified=True,
        source_sha256=pin["sha256"],
        files={str(p.relative_to(destination)): digest(p)
               for p in sorted(destination.rglob("*")) if p.is_file()},
    )
    (destination / "acquisition.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["plan", "pdfium", "verify", "stage"])
    parser.add_argument("path", nargs="*", type=Path)
    args = parser.parse_args()
    if args.command == "plan":
        print(json.dumps(source_pin(), indent=2))
    elif args.command == "pdfium":
        print(json.dumps(pdfium_pin(), indent=2))
    elif args.command == "verify":
        require(len(args.path) == 2, "verify expects ARCHIVE SIGNATURE_STATUS")
        verify_archive(args.path[0], args.path[1].read_text())
        print("verified")
    elif args.command == "stage":
        require(len(args.path) == 3, "stage expects ARCHIVE SIGNATURE_STATUS DEST")
        stage(args.path[0], args.path[1].read_text(), args.path[2])
        print("staged")


if __name__ == "__main__":
    main()
