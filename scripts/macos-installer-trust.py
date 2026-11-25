#!/usr/bin/env python3
"""Bounded final macOS installer trust evidence and offline publication gate.

`verify` observes macOS signing/notarization/Gatekeeper tools and writes scrubbed
schema-1 evidence. `check` only binds that evidence to the exact downloaded
package bytes; it never invokes a tool or the network and never prints a path.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

SCHEMA = 1
MAX_EVIDENCE_BYTES = 65536
MAX_TOOL_BYTES = 32768
MAX_TEXT_CHARS = 8192
MAX_PATH_CHARS = 4096
MAX_NOTE_CHARS = 256
TOOL_TIMEOUT_SECONDS = 30
PLATFORM = sys.platform  # module-level so offline tests can isolate verify()

STATUS_FIELDS = ("pkgutil_status", "stapler_status", "gatekeeper_status")
TEXT_FIELDS = ("pkgutil_text", "stapler_text", "gatekeeper_text")
FIELDS = frozenset(("schema", "artifact", "bytes", "sha256", "verified", "note")
                   + STATUS_FIELDS + TEXT_FIELDS)
NOTARIZED_STATUS = re.compile(
    r"Status:\s*signed by a developer certificate (?:issued by Apple for distribution"
    r"|that Apple notarized|notarized by Apple)",
    re.IGNORECASE,
)
NOTARIZATION_LINE = re.compile(
    r"Notarization:\s*trusted by the Apple notary service", re.IGNORECASE)
TRUSTED_TIMESTAMP = re.compile(r"^Signed with a trusted timestamp\b", re.IGNORECASE)
INSTALLER_CHAIN = re.compile(r"^\d+\.\s*Developer ID Installer:", re.IGNORECASE)
NEGATIVE_LINE = re.compile(r"^(?:Status|Notarization):", re.IGNORECASE)
BAD_STATUS = re.compile(
    r"\b(?:unsigned|unnotarized|not notarized|rejected|self-signed|no signature"
    r"|signed by (?:an? )?untrusted certificate"
    r"|signed by a certificate that has since expired)\b", re.IGNORECASE)
ACCEPTED_VERDICT = re.compile(r"(?:^|:)\s*accepted(?:\s|$)", re.IGNORECASE)


def clean(text):
    """Remove credentials and personal paths like release-audit.py's Audit.clean."""
    bases = []
    for key, replacement in (("GITHUB_WORKSPACE", "$WORKSPACE"),
                             ("RUNNER_TEMP", "$TEMP"), ("HOME", "$HOME"),
                             ("USERPROFILE", "$HOME")):
        value = os.environ.get(key)
        if value:
            bases.append((value, replacement))
    for value, replacement in ((str(Path(__file__).resolve().parent.parent), "$WORKSPACE"),
                               (str(Path.cwd().resolve()), "$WORKSPACE"),
                               (str(Path.home()), "$HOME")):
        if value:
            bases.append((value, replacement))
    expanded = set()
    for value, replacement in bases:
        expanded.add((value, replacement))
        forward, backward = value.replace("\\", "/"), value.replace("/", "\\")
        if forward != value:
            expanded.add((forward, replacement))
        if backward != value:
            expanded.add((backward, replacement))
        for variant in (forward, backward):
            if variant:
                expanded.add(("file://" + variant, replacement))
                expanded.add(("file:///" + variant.lstrip("/"), replacement))
    for value, replacement in sorted(expanded, key=lambda item: -len(item[0])):
        if value:
            text = text.replace(value, replacement)
    # Keep the structural marker needed by the parser, never the certificate owner.
    text = re.sub(r"(?im)^(.*\bDeveloper ID Installer:).*$", r"\1 <redacted>", text)
    text = re.sub(r"(?i)(keychain[- ]profile\s+)\S+", r"\1<redacted>", text)
    return re.sub(r"\(([A-Z0-9]{10})\)", "(<redacted>)", text)


def run_tool(argv):
    """Return (success, scrubbed bounded text, truncated) without leaking secrets."""
    try:
        result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                timeout=TOOL_TIMEOUT_SECONDS, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return False, "", True
    raw = result.stdout
    truncated = len(raw) > MAX_TOOL_BYTES
    text = clean(raw[:MAX_TOOL_BYTES].decode("utf-8", "replace"))[:MAX_TEXT_CHARS]
    return result.returncode == 0, text, truncated


def parse_pkgutil(success, text):
    """True only for a successful notarized, timestamped Developer ID Installer check."""
    if not success:
        return False
    signed = notarized = timestamped = installer = False
    for line in text.splitlines():
        stripped = line.strip()
        if NEGATIVE_LINE.match(stripped) and BAD_STATUS.search(stripped):
            return False
        if NOTARIZED_STATUS.fullmatch(stripped):
            signed = True
        if NOTARIZATION_LINE.fullmatch(stripped):
            notarized = True
        if TRUSTED_TIMESTAMP.match(stripped):
            timestamped = True
        if INSTALLER_CHAIN.match(stripped):
            installer = True
    return signed and notarized and timestamped and installer


def parse_stapler(success, text):
    """True only when the staple validation command exited successfully."""
    return success and "validate" in text.lower() and not re.search(
        r"\b(?:failed|invalid|error)\b", text, re.IGNORECASE)


def parse_gatekeeper(success, text):
    """True only for an explicit accepted verdict, never a rejected/unknown one."""
    if not success:
        return False
    lines = [line.strip() for line in text.splitlines()]
    if any("rejected" in line.lower() or "not accepted" in line.lower() for line in lines):
        return False
    return any(ACCEPTED_VERDICT.search(line) for line in lines)


def sha256_file(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_evidence(path, record):
    output = Path(path)
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("w", encoding="utf-8") as stream:
        json.dump(record, stream, sort_keys=True, indent=2, ensure_ascii=True)
        stream.write("\n")


def verify(args):
    if PLATFORM != "darwin":
        print("installer trust verify requires macOS tools", file=sys.stderr)
        return 1
    pkg = Path(args.pkg)
    size, digest, readable = 0, "", False
    try:
        if len(str(pkg)) > MAX_PATH_CHARS or not pkg.is_file():
            raise ValueError("installer package missing")
        size, digest, readable = pkg.stat().st_size, sha256_file(pkg), True
    except (OSError, ValueError):
        pass
    signature_ok, signature_text, signature_truncated = run_tool(
        ["pkgutil", "--check-signature", str(pkg)])
    staple_ok, staple_text, staple_truncated = run_tool(
        ["xcrun", "stapler", "validate", str(pkg)])
    gate_ok, gate_text, gate_truncated = run_tool(
        ["spctl", "-a", "-vvv", "-t", "install", str(pkg)])
    statuses = {
        "pkgutil_status": parse_pkgutil(signature_ok, signature_text),
        "stapler_status": parse_stapler(staple_ok, staple_text),
        "gatekeeper_status": parse_gatekeeper(gate_ok, gate_text),
    }
    verified = (readable and all(statuses.values())
                and not (signature_truncated or staple_truncated or gate_truncated))
    # Scrub again at the evidence boundary: no certificate owner, profile or path
    # is retained even if a tool adds one after its output was parsed.
    record = dict(
        schema=SCHEMA, artifact=clean(pkg.name), bytes=size, sha256=digest,
        pkgutil_text=clean(signature_text), stapler_text=clean(staple_text),
        gatekeeper_text=clean(gate_text),
        verified=verified, note=(
            "final installer signature, notarization and Gatekeeper acceptance verified"
            if verified else "final macOS installer trust not verified"),
    )
    record.update(statuses)
    try:
        write_evidence(args.output, record)
    except OSError as error:
        print("installer trust BLOCKED: evidence write failed", file=sys.stderr)
        return 1
    if not verified:
        print("installer trust BLOCKED: final installer trust not verified", file=sys.stderr)
        return 1
    print("installer trust evidence written")
    return 0


def parse_json(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate JSON key")
            result[key] = value
        return result
    return json.loads(raw.decode("utf-8"), object_pairs_hook=unique)


def validate_schema(record):
    if not isinstance(record, dict) or set(record) != FIELDS:
        raise ValueError("invalid installer trust schema")
    if type(record["schema"]) is not int or record["schema"] != SCHEMA:
        raise ValueError("unsupported installer trust schema")
    artifact = record["artifact"]
    if not isinstance(artifact, str) or not 0 < len(artifact) <= 255 or re.search(
            r"[/\\\x00]", artifact) or not artifact.isprintable():
        raise ValueError("invalid installer artifact name")
    if type(record["bytes"]) is not int or record["bytes"] < 0:
        raise ValueError("invalid installer byte count")
    if not isinstance(record["sha256"], str) or not re.fullmatch(
            r"[0-9a-f]{64}", record["sha256"]):
        raise ValueError("invalid installer sha256")
    for key in TEXT_FIELDS:
        if not isinstance(record[key], str) or len(record[key]) > MAX_TEXT_CHARS:
            raise ValueError("invalid installer trust text field")
    note = record["note"]
    if not isinstance(note, str) or len(note) > MAX_NOTE_CHARS:
        raise ValueError("invalid installer trust note")
    if record["verified"] is not True:
        raise ValueError("installer trust is not verified")
    for key in STATUS_FIELDS:
        if record[key] is not True:
            raise ValueError("installer trust " + key.replace("_status", "") + " not accepted")


def check(args):
    try:
        if len(str(args.pkg)) > MAX_PATH_CHARS:
            raise ValueError("installer path too long")
        raw = Path(args.evidence).read_bytes()
        if len(raw) > MAX_EVIDENCE_BYTES:
            raise ValueError("installer trust evidence too large")
        record = parse_json(raw)
        validate_schema(record)
        pkg = Path(args.pkg)
        if not pkg.is_file():
            raise ValueError("installer package missing")
        size, digest = pkg.stat().st_size, sha256_file(pkg)
        if size != record["bytes"]:
            raise ValueError("installer byte size mismatch")
        if digest != record["sha256"]:
            raise ValueError("installer sha256 mismatch")
        if pkg.name != record["artifact"]:
            raise ValueError("installer artifact name mismatch")
    except (OSError, ValueError, UnicodeDecodeError, json.JSONDecodeError) as error:
        print("installer trust BLOCKED: " + str(error))
        return 1
    print("installer trust PASS " + record["artifact"] + " sha256=" + record["sha256"])
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    producer = commands.add_parser("verify", help="observe macOS installer trust and write evidence")
    producer.add_argument("--pkg", required=True)
    producer.add_argument("--output", required=True)
    consumer = commands.add_parser("check", help="bind evidence to downloaded package bytes offline")
    consumer.add_argument("--pkg", required=True)
    consumer.add_argument("--evidence", required=True)
    args = parser.parse_args(argv)
    return verify(args) if args.command == "verify" else check(args)


if __name__ == "__main__":
    sys.exit(main())
