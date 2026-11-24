#!/usr/bin/env python3
"""Opt-in validation of the date-later Windows x64 PDFium candidate.

This helper is run only on a Windows validation runner. It downloads the
immutable chromium/8179 release asset and its attestation bundle over HTTPS,
checks both exact size/sha256 pins, verifies the asset's GitHub provenance with
`gh attestation verify` before any native bytes are extracted, and runs the
already-built preview smoke against a one-off stage containing the candidate
pdfium.dll, the caller's vcpkg FFmpeg DLL closure and the real rustc with its
sysroot DLLs. It never changes a
shipped pin, builds a product, publishes anything, or keeps native bytes: all
downloads, extraction and staging live in one TemporaryDirectory.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

sys.dont_write_bytecode = True

# Immutable opt-in pins. These are GitHub's reported digests for the immutable
# release; the shipped chromium/8138 pin is deliberately not derived from them.
RELEASE = "chromium/8179"
VERSION = "160.0.8179.0"
SOURCE_COMMIT = "02feff9f75486f5caac18147db45087b6ffbbec6"
ASSET = "pdfium-win-x64.tgz"
ASSET_BYTES = 4_046_589
ASSET_SHA256 = "836a77c9053f624839c66010cddea7b675628a36b8831d2330178bb169d241b6"
ATTESTATION_ASSET = "pdfium-attestation.json"
ATTESTATION_BYTES = 18_083
ATTESTATION_SHA256 = "f9d4bb0be84c30c98b98b2550ed2f81efc4024cd172511a5e786163c9ae8deb6"
REPO = "bblanchon/pdfium-binaries"
SIGNER_WORKFLOW = "github.com/bblanchon/pdfium-binaries/.github/workflows/build-all.yml"
SOURCE_REF = "refs/heads/master"
MAJOR = 160
BUILD = 8179
SHIPPED_RELEASE = "chromium/8138"

DOWNLOAD_RETRIES = 3
DOWNLOAD_TIMEOUT_SECONDS = 60
DOWNLOAD_RETRY_DELAY_SECONDS = 1
SMOKE_TIMEOUT_SECONDS = 300
GH_TIMEOUT_SECONDS = 120
RUSTC_TIMEOUT_SECONDS = 60
MAX_DOWNLOAD_RETRIES = 10
MAX_MEMBER_BYTES = 64 * 1024 * 1024
MAX_SMOKE_DLLS = 128
MAX_RUSTC_DLLS = 64
MAX_SMOKE_OUTPUT_CHARS = 200_000
SMOKE_ERROR_TAIL_CHARS = 2000
LATENCY_RECORDS = 5
MAX_EVIDENCE_BYTES = 20_000
SCOPE_NOTE = (
    "date-based candidate validation only; no exploitability, parity or "
    "release-readiness claim"
)


class CandidateError(RuntimeError):
    """A fail-closed validation error; the token is never part of its text."""


class HttpsRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Refuse a redirect that leaves HTTPS or adds credentials."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        parts = urllib.parse.urlsplit(newurl)
        if parts.scheme != "https" or parts.username or parts.password:
            raise CandidateError("download redirect must be HTTPS without credentials")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def require(condition, message):
    if not condition:
        raise CandidateError(message)


def release_urls():
    """Return the two immutable release URLs built only from the pins."""
    prefix = f"https://github.com/{REPO}/releases/download/{RELEASE}/"
    return prefix + ASSET, prefix + ATTESTATION_ASSET


def require_https(url):
    """Require the pinned release host and path before any network access."""
    parts = urllib.parse.urlsplit(url)
    require(parts.scheme == "https" and parts.hostname == "github.com",
            "PDFium candidate must be downloaded over HTTPS from github.com")
    require(parts.port is None and not parts.username and not parts.password,
            "PDFium candidate URL has an unexpected authority")
    require(not parts.query and not parts.fragment,
            "PDFium candidate URL has an unexpected query or fragment")
    require(parts.path.startswith(f"/{REPO}/releases/download/{RELEASE}/"),
            "PDFium candidate URL is outside the pinned release")
    return url


def file_pin(path, name, size, sha256):
    """Check an exact byte size and sha256; return a bounded evidence record."""
    path = Path(path)
    require(path.is_file(), f"missing pinned file: {name}")
    actual_size = path.stat().st_size
    require(actual_size == size,
            f"{name} size mismatch: got {actual_size}, expected {size}")
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while True:
            block = source.read(1024 * 1024)
            if not block:
                break
            digest.update(block)
    actual_sha256 = digest.hexdigest()
    require(actual_sha256 == sha256,
            f"{name} sha256 mismatch: got {actual_sha256}, expected {sha256}")
    return {"name": name, "bytes": size, "sha256": sha256}


def download(url, destination, *, retries=DOWNLOAD_RETRIES,
             delay=DOWNLOAD_RETRY_DELAY_SECONDS, opener=None):
    """Stream an HTTPS download to a fresh file, retrying failed attempts."""
    require_https(url)
    require(isinstance(retries, int) and 0 <= retries <= MAX_DOWNLOAD_RETRIES,
            "unsupported download retry count")
    destination = Path(destination)
    if opener is None:
        opener = lambda target, timeout: urllib.request.build_opener(
            HttpsRedirectHandler()).open(target, timeout=timeout)
    for attempt in range(retries + 1):
        try:
            with opener(url, timeout=DOWNLOAD_TIMEOUT_SECONDS) as response, \
                    destination.open("wb") as output:
                while True:
                    block = response.read(1024 * 1024)
                    if not block:
                        break
                    output.write(block)
            return destination
        except (OSError, urllib.error.URLError):
            destination.unlink(missing_ok=True)
            if attempt < retries and delay:
                time.sleep(delay)
    raise CandidateError(f"HTTPS download failed after {retries + 1} attempts")


def gh_command(asset, bundle):
    """Build the provenance command; the token is passed only through env."""
    return [
        "gh", "attestation", "verify", str(asset),
        "--repo", REPO,
        "--bundle", str(bundle),
        "--signer-workflow", SIGNER_WORKFLOW,
        "--signer-digest", SOURCE_COMMIT,
        "--source-digest", SOURCE_COMMIT,
        "--source-ref", SOURCE_REF,
    ]


def verify_attestation(asset, bundle, *, runner=subprocess.run, environ=None):
    """Bind the exact asset to the pinned bundle and upstream identity."""
    environ = os.environ if environ is None else environ
    token_values = [value.strip() for key, value in environ.items()
                    if key.upper() == "GH_TOKEN"]
    require(token_values, "GH_TOKEN is required for PDFium candidate attestation")
    token = environ.get("GH_TOKEN", token_values[0]).strip()
    require(token and all(value == token for value in token_values),
            "GH_TOKEN spellings must carry one consistent value")
    # Windows environment names are case-insensitive; drop every spelling so
    # the verifier sees exactly one GH_TOKEN value.
    env = {key: value for key, value in environ.items()
           if key.upper() != "GH_TOKEN"}
    env["GH_TOKEN"] = token
    command = gh_command(asset, bundle)
    require(not any(token in part for part in command),
            "GH_TOKEN must never appear in the verifier command")
    try:
        result = runner(command, env=env, capture_output=True, text=True,
                        timeout=GH_TIMEOUT_SECONDS, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise CandidateError("gh attestation verify could not run") from exc
    require(result.returncode == 0,
            f"gh attestation verify failed with exit code {result.returncode}")
    return {
        "status": "verified",
        "repo": REPO,
        "signer_workflow": SIGNER_WORKFLOW,
        "signer_digest": SOURCE_COMMIT,
        "source_digest": SOURCE_COMMIT,
        "source_ref": SOURCE_REF,
    }


def _member_name(member):
    """Return a safe POSIX-style member name or fail closed."""
    name = member.name
    require(name and "\\" not in name and "\x00" not in name,
            f"unsafe archive member: {name!r}")
    normalized = name.rstrip("/")
    parts = normalized.split("/")
    require(all(part not in ("", ".", "..") for part in parts),
            f"unsafe archive member: {name!r}")
    require(not normalized.startswith("/") and ":" not in parts[0],
            f"unsafe archive member: {name!r}")
    require(member.isfile() or member.isdir(),
            f"unsupported archive member type: {name!r}")
    if member.isfile():
        require(0 <= member.size <= MAX_MEMBER_BYTES,
                f"archive member exceeds the size bound: {name!r}")
    return normalized


def archive_layout(archive):
    """Validate the whole member list before any extraction happens."""
    names = set()
    with tarfile.open(archive, "r:gz") as tar:
        for member in tar.getmembers():
            name = _member_name(member)
            require(name not in names, f"duplicate archive member: {name}")
            names.add(name)
    dll = next((name for name in ("bin/pdfium.dll", "pdfium.dll")
                if name in names), None)
    require(dll is not None, "PDFium DLL member absent")
    for required in ("LICENSE", "VERSION"):
        require(required in names, f"PDFium closure member missing: {required}")
    licenses = tuple(sorted(name for name in names
                            if name.startswith("licenses/")
                            and not name.endswith("/")))
    require(licenses, "PDFium licenses/ closure absent")
    return {"dll": dll, "license": "LICENSE", "version": "VERSION",
            "licenses": licenses, "names": frozenset(names)}


def extract_required(archive, layout, destination):
    """Extract only the inspected regular files, into a private directory."""
    destination = Path(destination).resolve()
    outputs = {layout["dll"]: "pdfium.dll", "LICENSE": "LICENSE",
               "VERSION": "VERSION"}
    outputs.update((name, name) for name in layout["licenses"])
    with tarfile.open(archive, "r:gz") as tar:
        members = {_member_name(member): member for member in tar.getmembers()
                   if member.isfile()}
        for member_name, relative in sorted(outputs.items()):
            member = members.get(member_name)
            require(member is not None, f"unreadable archive member: {member_name}")
            target = (destination / relative).resolve()
            require(target != destination and destination in target.parents,
                    f"unsafe extraction target: {relative}")
            target.parent.mkdir(parents=True, exist_ok=True)
            source = tar.extractfile(member)
            require(source is not None, f"unreadable archive member: {member_name}")
            with source, target.open("wb") as output:
                written = 0
                while True:
                    block = source.read(1024 * 1024)
                    if not block:
                        break
                    written += len(block)
                    require(written <= member.size,
                            f"archive member grew during extraction: {member_name}")
                    output.write(block)
    return destination


def check_version(version_file):
    """Require the immutable candidate MAJOR and BUILD in VERSION."""
    raw = Path(version_file).read_bytes()
    require(len(raw) <= 4096, "VERSION file exceeds the size bound")
    lines = {line.strip() for line in raw.decode("ascii").splitlines()}
    require(f"MAJOR={MAJOR}" in lines and f"BUILD={BUILD}" in lines,
            "PDFium candidate VERSION does not match MAJOR=160 BUILD=8179")
    return {"major": MAJOR, "build": BUILD, "version": VERSION}


def rustc_sysroot(*, runner=subprocess.run):
    """Probe the real rustc sysroot before the smoke PATH is narrowed."""
    command = ["rustc", "--print", "sysroot"]
    try:
        result = runner(command, capture_output=True, text=True,
                        encoding="utf-8", errors="replace", check=False,
                        timeout=RUSTC_TIMEOUT_SECONDS)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise CandidateError("rustc sysroot probe could not run") from exc
    require(result.returncode == 0,
            f"rustc sysroot probe failed with exit code {result.returncode}")
    text = result.stdout.strip()
    require(0 < len(text) <= 4096, "rustc sysroot is empty or unbounded")
    sysroot = Path(text)
    require(sysroot.is_dir(), "rustc sysroot directory missing")
    return sysroot


def stage_candidate(work_dir, smoke_exe, dll_dir, candidate_dll, *,
                    sysroot=None, runner=subprocess.run):
    """Stage one isolated runtime; the only PDFium DLL is the candidate."""
    smoke_exe = Path(smoke_exe)
    dll_dir = Path(dll_dir)
    candidate_dll = Path(candidate_dll)
    require(smoke_exe.is_file(), "already-built preview smoke exe missing")
    require(smoke_exe.name.lower() == "preview_corpus_smoke.exe",
            "unexpected preview smoke executable name")
    require(dll_dir.is_dir(), "caller-supplied vcpkg DLL directory missing")
    require(candidate_dll.is_file(), "candidate pdfium.dll missing")
    dlls = sorted(path for path in dll_dir.iterdir()
                  if path.is_file() and path.suffix.lower() == ".dll")
    require(dlls, "caller-supplied vcpkg DLL closure is empty")
    require(len(dlls) <= MAX_SMOKE_DLLS, "caller-supplied DLL closure exceeds bounds")
    require(not any(path.name.lower().startswith("pdfium")
                    for path in dlls),
            "caller DLL closure must not contain a production PDFium DLL")
    # The shared smoke calls runtime_identity(), which invokes rustc. Stage the
    # compiler and its own DLL closure from the un-narrowed caller environment.
    sysroot = Path(sysroot) if sysroot is not None else rustc_sysroot(runner=runner)
    rustc_bin = sysroot / "bin"
    require(rustc_bin.is_dir(), "rustc sysroot bin directory missing")
    rustc_exe = rustc_bin / "rustc.exe"
    require(rustc_exe.is_file(), "rustc executable missing from sysroot bin")
    rustc_dlls = sorted(path for path in rustc_bin.iterdir()
                        if path.is_file() and path.suffix.lower() == ".dll")
    require(len(rustc_dlls) <= MAX_RUSTC_DLLS,
            "rustc sysroot DLL closure exceeds bounds")
    require(not any(path.name.lower().startswith("pdfium")
                    for path in rustc_dlls),
            "rustc sysroot DLL closure must not contain a production PDFium DLL")
    caller_names = [path.name.lower() for path in dlls]
    rustc_names = [path.name.lower() for path in rustc_dlls]
    require(len(set(caller_names)) == len(caller_names)
            and len(set(rustc_names)) == len(rustc_names)
            and not set(caller_names) & set(rustc_names),
            "staged runtime has duplicate DLL names")
    stage = Path(work_dir) / "stage"
    stage.mkdir(parents=True)
    shutil.copy2(smoke_exe, stage / "preview_corpus_smoke.exe")
    for dll in dlls:
        shutil.copy2(dll, stage / dll.name)
    shutil.copy2(candidate_dll, stage / "pdfium.dll")
    shutil.copy2(rustc_exe, stage / "rustc.exe")
    for dll in rustc_dlls:
        shutil.copy2(dll, stage / dll.name)
    return stage


def windows_system_path():
    """Pin the smoke PATH to Windows-provided directories only."""
    root_text = os.environ.get("SystemRoot") or os.environ.get("WINDIR", "")
    require(root_text and os.path.isabs(root_text),
            "SystemRoot must be an absolute Windows path")
    root = Path(root_text).resolve()
    candidates = (root / "System32", root,
                  root / "System32" / "WindowsPowerShell" / "v1.0")
    directories = []
    for candidate in candidates:
        resolved = candidate.resolve()
        require(resolved == root or root in resolved.parents,
                "smoke PATH directory escapes SystemRoot")
        require(resolved.is_dir(), f"Windows system directory missing: {resolved}")
        directories.append(resolved)
    return directories


def parse_preview_records(output):
    """Accept exactly one passing PREVIEW_CORRECTNESS record and five latency records."""
    records = []
    latency = 0
    for line in output.splitlines():
        if line.startswith("PREVIEW_CORRECTNESS "):
            payload = line[len("PREVIEW_CORRECTNESS "):]
            kind = "PREVIEW_CORRECTNESS"
        elif line.startswith("PREVIEW_LATENCY "):
            payload = line[len("PREVIEW_LATENCY "):]
            kind = "PREVIEW_LATENCY"
            latency += 1
        else:
            continue
        try:
            record = json.loads(payload)
        except ValueError as exc:
            raise CandidateError(f"invalid {kind} record JSON") from exc
        require(isinstance(record, dict), f"invalid {kind} record shape")
        if kind == "PREVIEW_CORRECTNESS":
            records.append(record)
    require(len(records) == 1,
            f"expected exactly one PREVIEW_CORRECTNESS record, got {len(records)}")
    require(records[0].get("passed") is True,
            "PREVIEW_CORRECTNESS record did not pass")
    require(latency == LATENCY_RECORDS,
            f"expected {LATENCY_RECORDS} PREVIEW_LATENCY records, got {latency}")
    return {"status": "pass", "passed": True, "correctness_records": 1,
            "latency_records": latency}


def smoke_output_tail(stdout, stderr):
    """Return the last bounded slice of the smoke's combined output."""
    return f"{stdout}\n{stderr}"[-SMOKE_ERROR_TAIL_CHARS:]


def run_smoke(stage, corpus, system_dirs, *, runner=subprocess.run):
    """Run the staged smoke with PATH limited to the stage and system dirs."""
    stage = Path(stage).resolve()
    corpus = Path(corpus).resolve()
    require(corpus.is_dir(), "already-fetched preview corpus missing")
    # Windows treats Path/PATH as the same variable; keep only one spelling,
    # otherwise a duplicate entry could reintroduce build or vcpkg directories.
    # The attestation token is not needed by the smoke and must never reach it.
    env = {key: value for key, value in os.environ.items()
           if key.upper() not in ("PATH", "GH_TOKEN")}
    env["PATH"] = os.pathsep.join([str(stage)] + [str(path) for path in system_dirs])
    try:
        result = runner([str(stage / "preview_corpus_smoke.exe"), str(corpus)],
                        cwd=str(stage), env=env, capture_output=True, text=True,
                        encoding="utf-8", errors="replace", check=False,
                        timeout=SMOKE_TIMEOUT_SECONDS)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise CandidateError("staged preview smoke could not run") from exc
    require(len(result.stdout) + len(result.stderr) <= MAX_SMOKE_OUTPUT_CHARS,
            "preview smoke output exceeds the evidence bound")
    require(result.returncode == 0,
            f"preview smoke failed with exit code {result.returncode}: "
            f"stdout/stderr tail: {smoke_output_tail(result.stdout, result.stderr)}")
    return parse_preview_records(result.stdout)


def write_evidence(path, asset_pin, attestation_pin, attestation, version, preview):
    """Write one bounded JSON evidence record; no native bytes or token."""
    report = {
        "schema": 1,
        "profile": "8179-opt-in",
        "release": RELEASE,
        "version": VERSION,
        "source_commit": SOURCE_COMMIT,
        "asset": asset_pin,
        "attestation": dict(attestation_pin, **attestation),
        "version_check": version,
        "preview": preview,
        "shipped_pin": {"release": SHIPPED_RELEASE, "changed": False},
        "scope": SCOPE_NOTE,
    }
    serialized = json.dumps(report, indent=2, sort_keys=True) + "\n"
    require(len(serialized.encode("utf-8")) <= MAX_EVIDENCE_BYTES,
            "candidate evidence exceeds the size bound")
    path = Path(path)
    require(not path.is_symlink(), "evidence path must not be a symlink")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(serialized, encoding="utf-8")


def verify(args):
    """Run the whole opt-in gate; TemporaryDirectory owns every native byte."""
    smoke_exe = Path(args.smoke_exe)
    dll_dir = Path(args.dll_dir)
    corpus = Path(args.corpus)
    require(smoke_exe.is_file() and dll_dir.is_dir() and corpus.is_dir(),
            "smoke exe, vcpkg DLL directory and fetched corpus must all exist")
    asset_url, bundle_url = release_urls()
    with tempfile.TemporaryDirectory(prefix="iris-pdfium-8179-") as temporary:
        work = Path(temporary)
        asset = work / ASSET
        bundle = work / ATTESTATION_ASSET
        download(asset_url, asset, retries=args.retries)
        download(bundle_url, bundle, retries=args.retries)
        asset_pin = file_pin(asset, ASSET, ASSET_BYTES, ASSET_SHA256)
        attestation_pin = file_pin(bundle, ATTESTATION_ASSET,
                                   ATTESTATION_BYTES, ATTESTATION_SHA256)
        attestation = verify_attestation(asset, bundle)
        layout = archive_layout(asset)
        extracted = work / "extract"
        extract_required(asset, layout, extracted)
        version = check_version(extracted / "VERSION")
        stage = stage_candidate(work, smoke_exe, dll_dir, extracted / "pdfium.dll")
        preview = run_smoke(stage, corpus, windows_system_path())
        write_evidence(Path(args.evidence), asset_pin, attestation_pin,
                       attestation, version, preview)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--smoke-exe", required=True, type=Path,
                        help="already-built preview_corpus_smoke.exe")
    parser.add_argument("--dll-dir", required=True, type=Path,
                        help="caller-supplied vcpkg FFmpeg DLL directory")
    parser.add_argument("--corpus", required=True, type=Path,
                        help="already-fetched preview corpus directory")
    parser.add_argument("--evidence", required=True, type=Path,
                        help="bounded JSON evidence path")
    parser.add_argument("--retries", type=int, default=DOWNLOAD_RETRIES,
                        help=f"HTTPS download retries (default {DOWNLOAD_RETRIES})")
    args = parser.parse_args()
    try:
        verify(args)
    except CandidateError as exc:
        print(f"PDFium 8179 candidate validation failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
