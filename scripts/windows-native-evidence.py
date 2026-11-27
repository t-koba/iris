#!/usr/bin/env python3
"""Prepare and seal reviewed Windows native-stage evidence for the release audit.

``prepare`` runs on the validating Windows runner after the release rehearsal.
It derives a native closure manifest from reviewed stage evidence and records
the exact hashes of every reviewed input and exported asset. ``seal`` runs in
the release consumer, where the generated checksum sidecars now exist: it
verifies those inputs against the downloaded bytes, adds the sidecars to the
closure and writes the strict manifest read by ``release-audit.py``.

This is evidence plumbing only: no build, no network, no native bytes and no
legal/compliance approval is produced here.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from release_evidence import (MODES, OBLIGATIONS, ffmpeg_component, load_json,
                              require, validate_closure)

MAX_BYTES = 2 * 1024 * 1024
MAX_FILES = 256
MAX_COMPONENTS = 4096
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
SAFE_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]*\Z")
COMPONENT_ID = re.compile(r"[a-z0-9][a-z0-9._-]*\Z")
INPUT_NAMES = ("validation-inputs.json", "pe-import-closure.json",
               "product-inventory.json")
ASSET_NAMES = ("Iris-windows.zip", "Iris-windows-setup.exe")
PRODUCER_FIELDS = {"schema", "artifacts", "components", "reviewed_inputs",
                   "assets", "expected_bytes"}
RESOLUTIONS = {"system", "stage", "vc-redist", "os", "unresolved"}


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def digest_ref(path):
    path = Path(path)
    return {"sha256": sha256(path), "bytes": path.stat().st_size}


def read_json(path):
    value = load_json(path, MAX_BYTES)
    require(isinstance(value, dict), "evidence must be a JSON object: " + str(path))
    return value


def require_file(path, label):
    path = Path(path)
    require(path.is_file() and not path.is_symlink(), label + " absent or unsupported")
    return path


def safe_path(value):
    require(isinstance(value, str) and value and "\x00" not in value
            and "\\" not in value and ":" not in value and not any(
                char in value for char in "\r\n\t"),
            "unsafe native evidence path")
    parts = value.split("/")
    require(not value.startswith("/") and all(part not in ("", ".", "..")
                                              for part in parts),
            "native evidence path must be exact and stage-relative")
    return value


def hash_ok(value):
    return isinstance(value, str) and SHA256.fullmatch(value) is not None


def coverage(notice_ref):
    return {mode: dict(components=[], evidence=[dict(notice_ref)])
            for mode in MODES}


def component(cid, ref, mode, name, origin, revision, license_id, disposition,
              modifications, notice_ref, pdfium=False):
    require(COMPONENT_ID.fullmatch(cid) is not None, "invalid native component id")
    obligations = {key: dict(status="provided", evidence=[dict(notice_ref)])
                   for key in OBLIGATIONS}
    if pdfium:
        obligations["relinking"]["status"] = "not-applicable"
    return dict(id=cid, artifact=dict(ref), mode=mode, name=name,
                source=dict(origin=origin, revision=revision,
                            evidence=[dict(notice_ref)]),
                modifications=dict(status=modifications, evidence=[dict(notice_ref)]),
                license=dict(id=license_id, disposition=disposition,
                             evidence=[dict(notice_ref)]),
                obligations=obligations)


def prepare(args):
    evidence_dir, release_dir, stage_dir = map(Path, (args.evidence, args.release, args.stage))
    source_pin = read_json(ROOT / "packaging/linux/ffmpeg.json")
    pdfium_pin = read_json(ROOT / "packaging/windows/ffmpeg.json")["pdfium"]
    values, reviewed = {}, {}
    for name in INPUT_NAMES:
        path = require_file(evidence_dir / name, "reviewed input " + name)
        values[name] = read_json(path)
        reviewed[name] = {"path": name, **digest_ref(path)}

    inputs, closure, inventory = (values[name] for name in INPUT_NAMES)
    require(inputs.get("source_version") == source_pin["version"]
            and inputs.get("source_sha256") == source_pin["sha256"]
            and inputs.get("source_url") == source_pin["url"]
            and inputs.get("source_fingerprint") == source_pin["fingerprint"],
            "validation inputs do not bind the approved FFmpeg source")
    require(inputs.get("pdfium_release") == pdfium_pin["release"]
            and inputs.get("pdfium_sha256") == pdfium_pin["sha256"],
            "validation inputs do not bind the approved PDFium asset")
    require(inputs.get("release_variant") == "product"
            and inputs.get("publishes_release_artifacts") is False
            and set(inputs.get("forbidden_features", [])) == {"gpl", "version3", "nonfree"}
            and set(inputs.get("lgpl_features", [])) >= {"avcodec", "avformat",
                                                        "avfilter", "avdevice",
                                                        "swresample", "swscale"},
            "validation inputs are not the reviewed product plan")
    require(closure.get("unresolved") == []
            and closure.get("vc_self_test", {}).get("status") == "pass"
            and isinstance(closure.get("imports"), list) and closure["imports"]
            and isinstance(closure.get("binaries"), list) and closure["binaries"]
            and set(closure.get("resolution_values", [])) == RESOLUTIONS,
            "PE import closure is not reviewed")
    require(all(isinstance(name, str) and SAFE_NAME.fullmatch(name) is not None
                for name in closure["binaries"]), "invalid PE import closure binary name")
    # The producer keys PE binaries case-insensitively, as Windows resolves them.
    closure_binaries = {name.lower() for name in closure["binaries"]}
    require({"iris.exe", "pdfium.dll"} <= closure_binaries,
            "PE import closure omits the required staged roots")

    files = inventory.get("files")
    require(isinstance(files, list) and 0 < len(files) <= MAX_FILES,
            "product inventory missing or oversized")
    products, expected_bytes = {}, {}
    for item in files:
        name = item.get("name") if isinstance(item, dict) else None
        require(isinstance(name, str) and SAFE_NAME.fullmatch(name) is not None
                and name not in products, "invalid/duplicate product inventory name")
        require(hash_ok(item.get("sha256")) and type(item.get("bytes")) is int
                and item["bytes"] > 0, "invalid product inventory binding")
        staged = require_file(stage_dir / name, "staged product file " + name)
        require(digest_ref(staged) == {"sha256": item["sha256"],
                                       "bytes": item["bytes"]},
                "product stage bytes differ from the reviewed inventory")
        products[name] = {"sha256": item["sha256"], "bytes": item["bytes"]}
        expected_bytes["Iris/" + name] = item["bytes"]
    require({"Iris.exe", "pdfium.dll", "FFMPEG-NOTICE.md"} <= set(products),
            "required product file missing from reviewed inventory")
    natives = [name for name in products if name.lower().endswith((".dll", ".exe"))]
    for name in natives:
        require(name.lower() in closure_binaries,
                "shipped native file absent from reviewed PE import closure: " + name)
    notice = require_file(stage_dir / "FFMPEG-NOTICE.md", "reviewed notice")
    with notice.open("rb") as stream:
        notice_bytes = stream.read(MAX_BYTES + 1)
    require(len(notice_bytes) <= MAX_BYTES, "reviewed notice exceeds bound")
    notice_text = notice_bytes.decode("utf-8", "replace")
    require("LGPL" in notice_text and source_pin["url"] in notice_text
            and pdfium_pin["sha256"] in notice_text,
            "reviewed notice lacks the native source/provenance record")
    notice_ref = {"path": "Iris/FFMPEG-NOTICE.md",
                  "sha256": products["FFMPEG-NOTICE.md"]["sha256"]}

    result = read_json(require_file(release_dir / "release-package-result.json",
                                    "release package result"))
    exported = read_json(require_file(release_dir / "product-inventory.json",
                                      "exported product inventory"))
    require(exported == inventory, "exported product inventory differs from reviewed inventory")
    require(result.get("status") == "pass"
            and sorted(result.get("entries", [])) == sorted(products)
            and result.get("installer_source_count") == len(products),
            "release package result does not match the reviewed inventory")
    assets = {}
    for name, key in (("Iris-windows.zip", "zip_sha256"),
                      ("Iris-windows-setup.exe", "installer_sha256")):
        require(hash_ok(result.get(key)), "invalid exported asset hash: " + name)
        path = require_file(release_dir / name, "exported asset " + name)
        binding = digest_ref(path)
        require(binding["sha256"] == result[key], "exported asset hash mismatch: " + name)
        assets[name] = binding
        expected_bytes[name] = binding["bytes"]
    copied = evidence_dir / "release-package-result.json"
    copied.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    reviewed["release-package-result.json"] = {"path": "release-package-result.json",
                                               **digest_ref(copied)}

    coverage_by_path, components = {}, []
    for name, binding in products.items():
        path = "Iris/" + name
        ref = {"path": path, "sha256": binding["sha256"]}
        low = name.lower()
        if low == "iris.exe":
            coverage_by_path.setdefault(path, {}).setdefault("dynamic", []).append(
                "iris-ffmpeg-dynamic")
            components.append(component(
                "iris-ffmpeg-dynamic", ref, "dynamic",
                "Iris.exe FFmpeg/PDFium dynamic imports", source_pin["url"],
                source_pin["sha256"], "LGPL-2.1-or-later", "review", "modified",
                notice_ref))
        elif low == "pdfium.dll":
            coverage_by_path.setdefault(path, {}).setdefault("dynamic", []).append("pdfium-dll")
            components.append(component(
                "pdfium-dll", ref, "dynamic", "PDFium dll", pdfium_pin["url"],
                pdfium_pin["sha256"], "BSD-3-Clause", "allowed", "unmodified",
                notice_ref, pdfium=True))
        elif low.endswith((".dll", ".exe")):
            # The shared recognizer decides both eligibility and the component
            # label, matching the audit's probe selection and policy checks.
            # IDs stay per artifact because the closure binds one artifact per
            # component record; both spellings of an FFmpeg library therefore
            # receive the same license component.
            ffmpeg = ffmpeg_component(name)
            require(ffmpeg is not None,
                    "no reviewed license component for shipped native file: " + name)
            cid = "ffmpeg-" + re.sub(r"[^a-z0-9._-]", "-", low)
            coverage_by_path.setdefault(path, {}).setdefault("dynamic", []).append(cid)
            components.append(component(
                cid, ref, "dynamic", "FFmpeg " + ffmpeg, source_pin["url"],
                source_pin["sha256"], "LGPL-2.1-or-later", "review", "modified",
                notice_ref))

    licensed = {row["artifact"]["path"] for row in components}
    require({"Iris/" + name for name in natives} <= licensed,
            "a shipped native file has no reviewed license component")
    artifacts = [dict(file={"path": name, "sha256": binding["sha256"]},
                      coverage=coverage(notice_ref))
                 for name, binding in assets.items()]
    for name, binding in products.items():
        path = "Iris/" + name
        row = coverage(notice_ref)
        row["dynamic"]["components"] = coverage_by_path.get(path, {}).get("dynamic", [])
        artifacts.append(dict(file={"path": path, "sha256": binding["sha256"]},
                              coverage=row))
    manifest = dict(schema=1, artifacts=artifacts, components=components,
                    reviewed_inputs=reviewed, assets=assets,
                    expected_bytes=expected_bytes)
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(dict(status="prepared", artifacts=len(artifacts),
                          components=len(components))))


def seal(args):
    producer = read_json(require_file(args.manifest, "producer evidence manifest"))
    require(set(producer) == PRODUCER_FIELDS and producer.get("schema") == 1,
            "unsupported producer evidence manifest")
    reviewed, assets, sizes = (producer[key] for key in
                               ("reviewed_inputs", "assets", "expected_bytes"))
    require(isinstance(reviewed, dict) and reviewed
            and isinstance(assets, dict) and set(assets) == set(ASSET_NAMES)
            and isinstance(sizes, dict), "incomplete producer evidence manifest")
    evidence_dir, asset_dir = Path(args.evidence), Path(args.assets)
    dist, product = Path(args.dist), Path(args.product)
    for name, binding in reviewed.items():
        require(isinstance(name, str) and SAFE_NAME.fullmatch(name) is not None,
                "unsafe reviewed evidence name")
        path = require_file(evidence_dir / name, "reviewed evidence " + name)
        require({"path": name, **digest_ref(path)} == binding,
                "reviewed evidence bytes differ: " + name)
        if name.endswith(".json"):
            read_json(path)
    for name, binding in assets.items():
        path = require_file(asset_dir / name, "exported asset " + name)
        require(digest_ref(path) == binding, "exported asset bytes differ: " + name)

    notice = require_file(product / "FFMPEG-NOTICE.md", "shipped notice")
    notice_ref = {"path": "Iris/FFMPEG-NOTICE.md", "sha256": sha256(notice)}
    artifacts, components = producer["artifacts"], producer["components"]
    require(isinstance(artifacts, list) and 0 < len(artifacts) <= MAX_FILES
            and isinstance(components, list) and len(components) <= MAX_COMPONENTS,
            "producer closure bounds exceeded")
    paths = []
    for artifact in artifacts:
        require(isinstance(artifact, dict) and set(artifact) == {"file", "coverage"},
                "invalid producer closure artifact")
        ref = artifact["file"]
        require(isinstance(ref, dict) and set(ref) == {"path", "sha256"}
                and hash_ok(ref["sha256"]), "invalid producer artifact reference")
        path = safe_path(ref["path"])
        require(path not in paths and type(sizes.get(path)) is int and sizes[path] > 0,
                "duplicate or unbounded producer artifact: " + path)
        if path in assets:
            require(ref["sha256"] == assets[path]["sha256"]
                    and sizes[path] == assets[path]["bytes"],
                    "producer asset binding differs from exported bytes: " + path)
        paths.append(path)
    licensed = {row["artifact"]["path"] for row in components
                if isinstance(row, dict) and isinstance(row.get("artifact"), dict)
                and isinstance(row["artifact"].get("path"), str)}
    for path in paths:
        if path.startswith("Iris/") and path.lower().endswith((".dll", ".exe")):
            require(path in licensed, "unlicensed shipped native file: " + path)
    require(any(a["file"] == notice_ref for a in artifacts),
            "reviewed notice is absent from the producer closure")

    require(dist.is_dir() and not dist.is_symlink(), "staged release directory absent")
    staged, pending = {}, [dist]
    while pending:
        with os.scandir(pending.pop()) as entries:
            for entry in entries:
                require(len(staged) < MAX_FILES, "staged release scan limit reached")
                path = Path(entry.path)
                require(not entry.is_symlink(), "unsupported staged link")
                if entry.is_dir(follow_symlinks=False):
                    pending.append(path)
                    continue
                require(entry.is_file(follow_symlinks=False)
                        and path.resolve().is_relative_to(dist.resolve()),
                        "unsupported/escaping staged file")
                rel = safe_path(path.relative_to(dist).as_posix())
                require(rel not in staged, "duplicate staged release path")
                staged[rel] = digest_ref(path)
    checksums = {name + ".sha256" for name in assets}
    require(set(staged) == set(paths) | checksums,
            "staged release files differ from the reviewed evidence")
    declared = {a["file"]["path"]: a["file"]["sha256"] for a in artifacts}
    for path in paths:
        require(staged[path] == {"sha256": declared[path], "bytes": sizes[path]},
                "staged release bytes differ from the reviewed evidence: " + path)
    for name, binding in assets.items():
        data = (dist / (name + ".sha256")).read_bytes()
        expected = (f"{binding['sha256']}  {name}\r\n").encode("ascii")
        if data != expected:
            expected = (f"{binding['sha256']}  {name}\n").encode("ascii")
        require(data == expected, "checksum sidecar differs from exported asset: " + name)
        artifacts.append(dict(file={"path": name + ".sha256",
                                    "sha256": staged[name + ".sha256"]["sha256"]},
                              coverage=coverage(notice_ref)))

    strict = dict(schema=1, artifacts=artifacts, components=components)
    records = [dict(kind="staged-file", path=path, **binding)
               for path, binding in staged.items()]
    native = dict(path="Iris/Iris.exe", **staged["Iris/Iris.exe"])
    records.append(dict(kind="native-file", **native))
    records.append(dict(kind="file", **native))
    validate_closure(strict, records, MAX_FILES, MAX_COMPONENTS)
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(strict, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(dict(status="sealed", artifacts=len(artifacts),
                          components=len(components))))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare")
    prepare_parser.add_argument("--evidence", required=True)
    prepare_parser.add_argument("--release", required=True)
    prepare_parser.add_argument("--stage", required=True)
    prepare_parser.add_argument("--output", required=True)
    prepare_parser.set_defaults(handler=prepare)
    seal_parser = commands.add_parser("seal")
    seal_parser.add_argument("--manifest", required=True)
    seal_parser.add_argument("--evidence", required=True)
    seal_parser.add_argument("--assets", required=True)
    seal_parser.add_argument("--dist", required=True)
    seal_parser.add_argument("--product", required=True)
    seal_parser.add_argument("--output", required=True)
    seal_parser.set_defaults(handler=seal)
    args = parser.parse_args()
    try:
        args.handler(args)
    except (OSError, ValueError, TypeError, KeyError, RecursionError) as error:
        print("Windows native evidence refused: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
