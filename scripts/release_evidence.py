"""Versioned native evidence structure, shared by collector and offline policy checker.

References are hashes of inventoried bytes, not URLs or assertions of completeness.
This validates structure and bindings, not the truth/legal sufficiency of documents.
"""
import json
from pathlib import Path
import re

MODES = ("static", "embedded", "dynamic", "dlopen")
OBLIGATIONS = ("notice", "source", "relinking")
FFMPEG_COMPONENTS = ("avcodec", "avformat", "avutil", "avdevice", "avfilter", "swscale", "swresample")
_POSIX_FFMPEG = re.compile(r"lib(?P<component>" + "|".join(FFMPEG_COMPONENTS) + r")[.-]")
_WINDOWS_FFMPEG = re.compile(r"(?P<component>" + "|".join(FFMPEG_COMPONENTS) + r")(?:-\d+)?\.dll", re.I)


def ffmpeg_component(path):
    """FFmpeg component named by an exact library filename, or None.

    POSIX spellings (libavcodec.so.60, libavcodec.60.dylib) and Windows DLL
    spellings (avcodec-62.dll) are both recognized. A name alone never proves
    identity: callers still bind the probed path to inventoried bytes.
    """
    if not isinstance(path, str):
        return None
    name = path.replace("\\", "/").rsplit("/", 1)[-1]
    match = _POSIX_FFMPEG.match(name) or _WINDOWS_FFMPEG.fullmatch(name)
    return match.group("component").lower() if match else None


def require(condition, message):
    if not condition:
        raise ValueError(message)


def text(value):
    return isinstance(value, str) and bool(value.strip())


def strings(value):
    return isinstance(value, list) and all(text(item) for item in value)


def parse_json(content):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "duplicate JSON key")
            result[key] = value
        return result
    return json.loads(content, object_pairs_hook=unique)


def load_json(path, max_bytes):
    with Path(path).open("rb") as stream:
        raw = stream.read(max_bytes + 1)
    require(len(raw) <= max_bytes, "JSON exceeds configured byte limit")
    return parse_json(raw.decode("utf-8"))


def fields(value, keys):
    require(isinstance(value, dict) and set(value) == set(keys.split()),
            "missing/unsupported native evidence fields: " + keys)


def inventory(records, kind):
    """Index byte identities without discarding repeated observation/provenance records."""
    result = {}
    for row in records:
        if row["kind"] != kind:
            continue
        path, digest = row.get("path"), row.get("sha256")
        require(text(path) and isinstance(digest, str) and re.fullmatch(r"[0-9a-f]{64}", digest),
                "invalid " + kind)
        require(type(row.get("bytes")) is int and row["bytes"] >= 0, "invalid inventory size")
        binding = (digest, row["bytes"])
        require(path not in result or result[path] == binding, "conflicting " + kind + " hash/size binding")
        result[path] = binding
    return {path: binding[0] for path, binding in result.items()}


def validate_closure(value, records, max_files, max_components):
    fields(value, "schema artifacts components")
    require(type(value["schema"]) is int and value["schema"] == 1, "unsupported native evidence schema")
    staged, native = inventory(records, "staged-file"), inventory(records, "native-file")
    files = inventory(records, "file")
    require(staged and native and len(staged.keys() | native.keys()) <= max_files,
            "missing/oversized staged/native inventory")
    for path, digest in native.items():
        require(files.get(path) == digest, "native inventory lacks matching file hash")
    bindings = {}
    for row in records:
        if row["kind"] in ("file", "native-file", "staged-file"):
            binding = (row["sha256"], row["bytes"])
            require(row["path"] not in bindings or bindings[row["path"]] == binding,
                    "inconsistent inventory hash/size binding")
            bindings[row["path"]] = binding
    targets = staged | native
    supporting = staged | files
    sizes = {path: binding[1] for path, binding in bindings.items()}

    def reference(ref, catalog):
        fields(ref, "path sha256")
        require(text(ref["path"]) and ref["path"] in catalog
                and ref["sha256"] == catalog[ref["path"]], "missing/mismatched native evidence reference")
        return ref["path"]

    def evidence(refs):
        require(isinstance(refs, list) and refs, "missing native supporting evidence")
        paths = [reference(ref, supporting) for ref in refs]
        require(len(paths) == len(set(paths)) and all(sizes[path] > 0 for path in paths),
                "duplicate/empty native supporting evidence")

    artifacts, components = value["artifacts"], value["components"]
    require(isinstance(artifacts, list) and len(artifacts) == len(targets)
            and isinstance(components, list) and components and len(components) <= max_components,
            "missing/oversized native artifacts/components")
    coverage = {}
    for artifact in artifacts:
        fields(artifact, "file coverage")
        path = reference(artifact["file"], targets)
        require(path not in coverage, "duplicate native artifact")
        fields(artifact["coverage"], " ".join(MODES))
        coverage[path] = {}
        for mode, row in artifact["coverage"].items():
            fields(row, "components evidence")
            ids = row["components"]
            require(strings(ids) and len(ids) == len(set(ids)), "invalid/duplicate coverage components")
            evidence(row["evidence"])  # Empty dependency lists still need inventory/link/loader evidence.
            coverage[path][mode] = set(ids)
    require(set(coverage) == set(targets), "omitted native artifact")
    seen, identities = set(), set()
    for component in components:
        fields(component, "id artifact mode name source modifications license obligations")
        cid, mode = component["id"], component["mode"]
        require(text(cid) and cid not in seen and text(component["name"]) and mode in MODES,
                "invalid/duplicate native component")
        path = reference(component["artifact"], targets)
        identity = (path, mode, component["name"])
        require(identity not in identities and cid in coverage[path][mode], "duplicate/uncovered native component")
        identities.add(identity)
        seen.add(cid)
        coverage[path][mode].remove(cid)
        source = component["source"]
        fields(source, "origin revision evidence")
        require(text(source["origin"]) and text(source["revision"]), "missing native source/revision")
        evidence(source["evidence"])
        modifications = component["modifications"]
        fields(modifications, "status evidence")
        require(modifications["status"] in ("unmodified", "modified"), "unknown vendor modifications")
        evidence(modifications["evidence"])  # Patch set, or evidence of an unmodified source comparison.
        license_row = component["license"]
        fields(license_row, "id disposition evidence")
        require(text(license_row["id"]) and license_row["disposition"] in ("allowed", "review"),
                "unknown native license disposition")
        evidence(license_row["evidence"])
        fields(component["obligations"], " ".join(OBLIGATIONS))
        for obligation in component["obligations"].values():
            fields(obligation, "status evidence")
            require(obligation["status"] in ("provided", "not-applicable"), "unknown native obligation")
            evidence(obligation["evidence"])  # Inapplicability requires a documented rationale too.
    require(not any(ids for modes in coverage.values() for ids in modes.values()),
            "missing native component records")
    return components
