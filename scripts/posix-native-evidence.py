#!/usr/bin/env python3
"""Generate a reviewed native-closure sidecar for a macOS or Linux release stage.

The producer only reads the exact staged tree and checked-in mapping/pin
documents. It writes its JSON outside ``dist/<platform>`` and never builds,
signs, patches or copies release bytes. Every staged file must match one
reviewed mapping rule, every native file must have a reviewed component, and
the generated closure is self-checked with the same validator the audit uses.

This is evidence plumbing, not legal/compliance approval or a release decision.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from release_evidence import (MODES, OBLIGATIONS, ffmpeg_component,
                              load_json, require, validate_closure)

MAX_BYTES = 2 * 1024 * 1024
MAPPING_FIELDS = ("schema platform scope pins evidence_files staged_rules "
                  "components system_components")
COMPONENT_FIELDS = ("name pin license disposition mode modifications "
                    "obligations evidence")
SYSTEM_FIELDS = ("name license disposition mode package obligations")
SAFE_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]*\Z")
CID = re.compile(r"[a-z0-9][a-z0-9._-]*\Z")
MACHO = (b"\xfe\xed\xfa\xce", b"\xfe\xed\xfa\xcf", b"\xce\xfa\xed\xfe",
         b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
DISPOSITIONS = ("allowed", "review")
MODIFICATIONS = ("unmodified", "modified")
RUNTIME_REPORT = "Iris.app/Contents/Resources/ffmpeg-runtime.json"
BUILD_MANIFEST = "Iris.app/Contents/Resources/ffmpeg/manifest.json"
LZMA_SYMBOLS = ("_lzma_code", "_lzma_end", "_lzma_stream_decoder", "_lzma_stream_buffer_decode")


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def binding(path):
    path = Path(path)
    return {"sha256": sha256(path), "bytes": path.stat().st_size}


def reference(path, digest):
    return {"path": path, "sha256": digest}


def read_json(path):
    value = load_json(path, MAX_BYTES)
    require(isinstance(value, dict), "expected a JSON object: " + str(path))
    return value


def safe_relative(value):
    require(isinstance(value, str) and value and "\x00" not in value
            and "\\" not in value and ":" not in value
            and not any(char in value for char in "\r\n\t"),
            "unsafe staged evidence path")
    parts = value.split("/")
    require(not value.startswith("/") and "//" not in value
            and not any(part in ("", ".", "..") for part in parts),
            "staged evidence path must be exact and relative: " + value)
    return value


def compile_rule(pattern):
    """A rule is a glob over path segments; ``**`` may cross directories."""
    safe_relative(pattern)
    require("?" not in pattern and "[" not in pattern, "unsupported rule glob: " + pattern)
    expression = re.escape(pattern).replace(r"\*\*", ".*").replace(r"\*", "[^/]*")
    return re.compile(r"\A" + expression + r"\Z")


def walk_stage(stage, max_files):
    """Hash every stage-relative file the audit will inventory, symlinks included."""
    require(stage.is_dir() and not stage.is_symlink(), "release stage absent or unsupported")
    root = stage.resolve()
    found, pending = {}, [root]
    while pending:
        with os.scandir(pending.pop()) as entries:
            for entry in entries:
                path = Path(entry.path)
                if entry.is_dir(follow_symlinks=False):
                    pending.append(path)
                    continue
                require(entry.is_file(follow_symlinks=True)
                        and path.resolve().is_relative_to(root),
                        "unsupported/escaping staged file: " + str(path))
                relative = path.relative_to(root).as_posix()
                require(len(found) < max_files, "staged native-evidence scan limit reached")
                require(relative not in found, "duplicate staged path: " + relative)
                found[relative] = binding(path)
    return dict(sorted(found.items()))


def native_bytes(path):
    with Path(path).open("rb") as stream:
        head = stream.read(4)
    return head.startswith(b"\x7fELF") or head in MACHO


def source_pins(mapping, platform):
    pins = mapping["pins"]
    require(isinstance(pins, dict), "invalid reviewed pin set")
    result = {}
    for name, location in pins.items():
        require(name in ("ffmpeg", "liblzma", "pdfium") and isinstance(location, str)
                and not Path(location).is_absolute() and ".." not in Path(location).parts,
                "unsupported reviewed pin: " + str(name))
        pin = read_json(ROOT / location)
        if name == "pdfium":
            require(platform == "linux", "PDFium pin only applies to Linux")
            row = pin.get("pdfium", pin)
            require(isinstance(row, dict), "invalid PDFium pin document")
        else:
            require(platform == "macos" or name == "ffmpeg", "unsupported pin for platform")
            row = pin
        origin = row.get("url")
        revision = row.get("sha256")
        require(isinstance(origin, str) and origin.startswith("https://")
                and isinstance(revision, str) and SHA256.fullmatch(revision) is not None,
                "invalid reviewed source pin: " + name)
        result[name] = (origin, revision)
    require("ffmpeg" in result and (platform != "macos" or "liblzma" in result),
            "reviewed mapping omits required source pins")
    return result


def component_template(template, components, pins, evidence_names, name, mode="dynamic"):
    require(isinstance(template, str) and template in components,
            "no reviewed component rule for staged native file: " + name)
    spec = components[template]
    require(isinstance(spec, dict) and set(spec) == set(COMPONENT_FIELDS.split()),
            "invalid reviewed component: " + template)
    require(isinstance(spec["name"], str) and spec["name"].strip(),
            "invalid reviewed component name: " + template)
    require(spec["pin"] in pins, "unreviewed source pin for component: " + template)
    licenses = ("LGPL-2.1-or-later", "BSD-3-Clause") if mode == "dynamic" else ("0BSD",)
    require(spec["license"] in licenses
            and spec["disposition"] in DISPOSITIONS
            and spec["mode"] == mode
            and spec["modifications"] in MODIFICATIONS,
            "invalid reviewed native component: " + template)
    if mode == "static":
        # Static mode is only for the reviewed, unmodified pinned liblzma.
        require("{" not in template and spec["pin"] == "liblzma"
                and spec["license"] == "0BSD" and spec["disposition"] == "allowed"
                and spec["modifications"] == "unmodified",
                "invalid reviewed static component: " + template)
    require(isinstance(spec["obligations"], list)
            and len(spec["obligations"]) == len(set(spec["obligations"]))
            and set(spec["obligations"]) <= set(OBLIGATIONS),
            "invalid reviewed obligations: " + template)
    require(isinstance(spec["evidence"], list) and spec["evidence"]
            and len(spec["evidence"]) == len(set(spec["evidence"]))
            and set(spec["evidence"]) <= set(evidence_names),
            "unreviewed component evidence: " + template)
    if "{basename}" in template:
        require(ffmpeg_component(name) is not None,
                "no reviewed license identity for staged native file: " + name)
        cid = template.replace("{basename}", re.sub(r"[^a-z0-9._-]", "-", name.lower()))
    else:
        cid = template
    if mode == "static":
        cid += "-" + re.sub(r"[^a-z0-9._-]", "-", name.lower())
    require(CID.fullmatch(cid) is not None, "invalid generated component id: " + cid)
    return cid, spec


def review_mapping(path, platform):
    mapping = read_json(path)
    require(set(mapping) == set(MAPPING_FIELDS.split()), "unsupported native evidence mapping")
    require(mapping["schema"] == 1 and mapping["platform"] == platform
            and isinstance(mapping["scope"], str) and mapping["scope"].strip(),
            "native evidence mapping does not match this release platform")
    evidence = mapping["evidence_files"]
    require(isinstance(evidence, list) and evidence and len(evidence) <= 16
            and len(set(evidence)) == len(evidence),
            "invalid reviewed supporting evidence set")
    for name in evidence:
        safe_relative(name)
    components = mapping["components"]
    systems = mapping["system_components"]
    rules = mapping["staged_rules"]
    require(isinstance(rules, list) and 0 < len(rules) <= 256, "invalid staged rule set")
    for rule in rules:
        require(isinstance(rule, dict) and set(rule) - {"static_component"} in (
            {"path", "role"}, {"path", "role", "component"}), "invalid staged rule")
        compile_rule(rule["path"])
        require(rule["role"] in ("native", "evidence", "data", "asset"),
                "unknown stage evidence role")
        if "static_component" in rule:
            require(rule["role"] == "native" and "component" in rule
                    and isinstance(rule["static_component"], str)
                    and rule["static_component"] in components
                    and rule["static_component"] != rule["component"],
                    "invalid reviewed static component rule")
    require(isinstance(components, dict) and isinstance(systems, dict),
            "invalid reviewed component set")
    for row in systems.values():
        require(isinstance(row, dict) and set(row) == set(SYSTEM_FIELDS.split()),
                "invalid reviewed system component")
        require(isinstance(row["name"], str) and row["name"].strip()
                and row["license"] in ("LGPL-2.1-or-later", "Zlib", "0BSD", "BSD-3-Clause",
                                       "GPL-3.0-or-later WITH GCC-exception-3.1")
                and row["disposition"] in DISPOSITIONS and row["mode"] == "dynamic"
                and SAFE_NAME.fullmatch(row["package"]) is not None,
                "invalid system license mapping")
        require(isinstance(row["obligations"], dict)
                and set(row["obligations"]) == set(OBLIGATIONS)
                and all(row["obligations"][key] in ("provided", "not-applicable")
                        for key in OBLIGATIONS),
                "invalid system obligation mapping")
    return mapping


def run_tool(argv, seconds=30):
    try:
        completed = subprocess.run(argv, capture_output=True, text=True,
                                   timeout=seconds, check=False)
    except (OSError, subprocess.SubprocessError) as error:
        raise ValueError("native dependency tool unavailable: " + str(error)) from error
    require(completed.returncode == 0,
            "native dependency inspection failed: " + " ".join(argv))
    return completed.stdout


def ldd_dependencies(path):
    listing = run_tool(["ldd", str(path)])
    result = set()
    for line in listing.splitlines():
        row = line.strip()
        require("not found" not in row, "unresolved ELF dependency: " + row)
        match = re.match(r"(?:\S+\s+=>\s+)?(/\S+)\s+\(", row)
        if match:
            result.add(Path(match.group(1)).resolve())
        elif row.startswith("/") and "(" in row:
            result.add(Path(row.split("(", 1)[0].strip()).resolve())
    return result


def otool_dependencies(path):
    listing = run_tool(["otool", "-L", str(path)])
    loads = run_tool(["otool", "-l", str(path)])
    rpaths = re.findall(r"cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset", loads)
    result = set()

    def expand(value):
        return value.replace("@loader_path", str(path.parent)).replace(
            "@executable_path", str(path.parent))

    for line in listing.splitlines()[1:]:
        dep = line.strip().split(" (", 1)[0]
        if dep.startswith("@rpath/"):
            candidates = [Path(expand(p)) / dep[len("@rpath/"):] for p in rpaths]
            resolved = next((p for p in candidates if p.is_file()), None)
            require(resolved is not None, "unresolved @rpath dependency: " + dep)
        elif dep.startswith(("@loader_path/", "@executable_path/")):
            resolved = Path(expand(dep))
        elif dep.startswith("/"):
            resolved = Path(dep)
        else:
            raise ValueError("unresolved Mach-O install name: " + dep)
        if str(resolved).startswith(("/System/", "/usr/lib/")):
            continue  # audit records these as system-dependency, not native-file
        require(resolved.is_file(), "unresolved non-system Mach-O dependency: " + dep)
        result.add(resolved.resolve())
    return result


def external_dependencies(platform, stage, staged, max_files):
    """System/third-party DSOs the audit will add as native-file inventory."""
    root = stage.resolve()
    extensions = ldd_dependencies if platform == "linux" else otool_dependencies
    pending = [root / name for name, row in staged.items()
               if row["bytes"] > 0 and native_bytes(root / name)]
    seen, external = set(), {}
    while pending:
        path = pending.pop().resolve()
        if path in seen:
            continue
        seen.add(path)
        require(len(seen) <= max_files, "native dependency scan limit reached")
        for dependency in extensions(path):
            dependency = dependency.resolve()
            if dependency.is_relative_to(root):
                continue  # staged files are already covered by the stage walk
            if dependency not in external:
                require(dependency.is_file() and dependency.stat().st_size > 0,
                        "unusable external native file: " + str(dependency))
                external[dependency] = binding(dependency)
                pending.append(dependency)
    return dict(sorted(external.items(), key=lambda item: str(item[0])))


def system_package(path):
    owner = run_tool(["dpkg-query", "-S", str(path)])
    for line in owner.splitlines():
        name, separator, _ = line.partition(": /")
        if separator:
            return name.split(":", 1)[0]
    raise ValueError("no Debian package owner for external native file: " + str(path))


def lzma_pin(mapping):
    """Read the reviewed static liblzma pin referenced by the mapping."""
    location = mapping["pins"].get("liblzma")
    require(isinstance(location, str) and not Path(location).is_absolute()
            and ".." not in Path(location).parts,
            "reviewed mapping omits the static liblzma pin")
    pin = read_json(ROOT / location)
    license = pin.get("license")
    require(isinstance(license, dict) and license.get("scope") == "liblzma"
            and license.get("spdx") == "0BSD"
            and SHA256.fullmatch(str(license.get("sha256", ""))) is not None
            and isinstance(pin.get("version"), str) and pin["version"]
            and SHA256.fullmatch(str(pin.get("sha256", ""))) is not None,
            "invalid reviewed static liblzma pin")
    return pin


def nm_symbols(path):
    """Re-inspect the staged dylib itself; the report never proves its bytes."""
    listing = run_tool(["nm", "-gU", str(path)])
    symbols = set()
    for line in listing.splitlines():
        parts = line.split()
        if (len(parts) >= 3 and re.fullmatch(r"[0-9a-fA-F]+", parts[0])
                and parts[1] in "TtDdBbRrWwVvSsIiNn" and parts[-1] in LZMA_SYMBOLS):
            symbols.add(parts[-1])
    return sorted(symbols)


def static_lzma_runtime(mapping, stage, staged):
    """Bind pinned liblzma build evidence to the staged runtime and dylibs."""
    pin = lzma_pin(mapping)
    runtime = read_json(stage / RUNTIME_REPORT)
    build_manifest = read_json(stage / BUILD_MANIFEST)
    evidence = runtime.get("liblzma")
    require(isinstance(evidence, dict) and evidence == build_manifest.get("liblzma"),
            "runtime and build liblzma evidence differ")
    signature = evidence.get("signature")
    # The build report nests the full reviewed pin under "source" (version,
    # source hash and license metadata); there are no redundant top-level keys.
    require(isinstance(signature, dict) and evidence.get("source") == pin
            and signature.get("verified") is True
            and signature.get("primary_fingerprint") == pin.get("fingerprint")
            and SHA256.fullmatch(str(evidence.get("license_metadata_sha256", ""))) is not None
            and SHA256.fullmatch(str(evidence.get("static_archive_sha256", ""))) is not None,
            "runtime liblzma evidence does not match the reviewed pin")
    documents = build_manifest.get("documents")
    require(isinstance(documents, dict), "build manifest lacks staged document hashes")
    for filename, expected in (("xz-source.tar.xz", pin["sha256"]),
                               ("xz-COPYING.0BSD", pin["license"]["sha256"])):
        path = "Iris.app/Contents/Resources/ffmpeg/" + filename
        require(staged.get(path, {}).get("sha256") == expected
                and documents.get(filename) == expected,
                "staged liblzma evidence differs from the reviewed pin: " + filename)
    path = "Iris.app/Contents/Resources/ffmpeg/liblzma-build.json"
    require(staged.get(path, {}).get("sha256") == documents.get("liblzma-build.json"),
            "staged liblzma build evidence differs from the build manifest")
    staged_libraries = build_manifest.get("staged_libraries")
    libraries = runtime.get("static_libraries")
    require(isinstance(staged_libraries, dict) and isinstance(libraries, dict) and libraries,
            "runtime lacks verified static library evidence")
    return libraries, staged_libraries


def build(args):
    max_files = int(os.environ.get("IRIS_RELEASE_AUDIT_MAX_NATIVE_FILES", "256"))
    max_components = int(os.environ.get("IRIS_RELEASE_AUDIT_MAX_NATIVE_COMPONENTS", "4096"))
    require(min(max_files, max_components) > 0, "invalid native evidence bounds")
    stage = Path(args.stage)
    output = Path(args.output)
    require(not output.resolve().is_relative_to(stage.resolve()),
            "native evidence sidecar must be written outside the release stage")
    mapping_path = Path(args.mapping)
    if not mapping_path.is_absolute():
        mapping_path = ROOT / mapping_path
    require(mapping_path.resolve().is_relative_to(ROOT),
            "reviewed mapping must be inside the repository: " + str(mapping_path))
    mapping = review_mapping(mapping_path, args.platform)
    pins = source_pins(mapping, args.platform)
    staged = walk_stage(stage, max_files)
    require(staged, "empty release stage")
    for name in mapping["evidence_files"]:
        require(name in staged and staged[name]["bytes"] > 0,
                "reviewed supporting evidence absent from stage: " + name)
    primary = reference(mapping["evidence_files"][0],
                        staged[mapping["evidence_files"][0]]["sha256"])
    rules = [(compile_rule(rule["path"]), rule) for rule in mapping["staged_rules"]]
    static_rules = any("static_component" in rule for _, rule in rules)
    static_context = static_lzma_runtime(mapping, stage, staged) if args.platform == "macos" and static_rules else None
    components, staged_components = [], {}
    static_count = 0
    for name, row in staged.items():
        matches = [rule for pattern, rule in rules if pattern.fullmatch(name)]
        require(len(matches) == 1, "unmapped staged file: " + name)
        rule = matches[0]
        native = native_bytes(stage / name)
        require(native == (rule["role"] == "native"),
                "staged native bytes have no reviewed native rule: " + name)
        if native:
            cid, spec = component_template(rule["component"], mapping["components"], pins,
                                           mapping["evidence_files"], name)
            origin, revision = pins[spec["pin"]]
            reviewed_refs = [reference(path, staged[path]["sha256"])
                             for path in spec["evidence"]]
            obligations = {key: dict(status="provided", evidence=reviewed_refs)
                           for key in OBLIGATIONS}
            components.append(dict(
                id=cid, artifact=reference(name, row["sha256"]), mode=spec["mode"],
                name=spec["name"],
                source=dict(origin=origin, revision=revision, evidence=reviewed_refs),
                modifications=dict(status=spec["modifications"], evidence=reviewed_refs),
                license=dict(id=spec["license"], disposition=spec["disposition"],
                             evidence=reviewed_refs),
                obligations=obligations))
            staged_components.setdefault(name, []).append((cid, spec["mode"]))
            if "static_component" in rule:
                require(static_context is not None,
                        "static native component requires staged macOS runtime evidence")
                libraries, staged_libraries = static_context
                basename = Path(name).name  # Runtime report keys are staged dylib basenames.
                entry = libraries.get(basename)
                # The staged bytes, not the report's key set, decide which dylibs
                # contain liblzma. A truncated report must not leave a containing
                # dylib with empty static coverage.
                measured = nm_symbols(stage / name)
                if len(measured) < 2:
                    require(entry is None,
                            "static liblzma symbols absent from the staged dylib: " + name)
                    continue
                if entry is not None:
                    require(isinstance(entry, dict) and set(entry) == {"sha256", "symbols"},
                            "invalid runtime static library evidence: " + name)
                    symbols = entry["symbols"]
                    require(isinstance(symbols, list) and 2 <= len(symbols) <= len(LZMA_SYMBOLS)
                            and len(set(symbols)) == len(symbols)
                            and set(symbols) <= set(LZMA_SYMBOLS),
                            "unreviewed static liblzma symbols: " + name)
                    require(measured == sorted(symbols),
                            "static liblzma symbols differ from the staged dylib: " + name)
                    require(entry["sha256"] == row["sha256"] == staged_libraries.get(basename),
                            "static liblzma bytes differ from the staged dylib: " + name)
                static_cid, static_spec = component_template(
                    rule["static_component"], mapping["components"], pins,
                    mapping["evidence_files"], name, mode="static")
                static_refs = [reference(path, staged[path]["sha256"])
                               for path in static_spec["evidence"]]
                components.append(dict(
                    id=static_cid, artifact=reference(name, row["sha256"]), mode="static",
                    name=static_spec["name"],
                    source=dict(origin=pins[static_spec["pin"]][0],
                                revision=pins[static_spec["pin"]][1], evidence=static_refs),
                    modifications=dict(status=static_spec["modifications"], evidence=static_refs),
                    license=dict(id=static_spec["license"], disposition=static_spec["disposition"],
                                 evidence=static_refs),
                    obligations={key: dict(status="provided", evidence=static_refs)
                                 for key in OBLIGATIONS}))
                staged_components[name].append((static_cid, "static"))
                static_count += 1
    if static_rules:
        require(static_count > 0, "no static liblzma component bound to staged bytes")
    require(len({row["id"] for row in components}) == len(components),
            "duplicate reviewed native component id")

    external = external_dependencies(args.platform, stage, staged, max_files)
    external_components = {}
    for path, row in external.items():
        rule = mapping["system_components"].get(path.name)
        require(rule is not None, "no reviewed component for external native file: " + str(path))
        if args.platform == "linux":
            require(system_package(path) == rule["package"],
                    "external native file package differs from reviewed mapping: " + str(path))
        cid = "system-" + re.sub(r"[^a-z0-9._-]", "-", path.name.lower())
        require(CID.fullmatch(cid) is not None, "invalid external native component id")
        external_components[path] = (cid, rule["mode"])
        components.append(dict(
            id=cid, artifact=reference(str(path), row["sha256"]), mode=rule["mode"],
            name=rule["name"],
            source=dict(origin="reviewed OS package: " + rule["package"],
                        revision="installed package bytes", evidence=[primary]),
            modifications=dict(status="unmodified", evidence=[primary]),
            license=dict(id=rule["license"], disposition=rule["disposition"],
                         evidence=[primary]),
            obligations={key: dict(status=rule["obligations"][key], evidence=[primary])
                         for key in OBLIGATIONS}))
    require(len(components) <= max_components, "native component bound exceeded")

    def coverage(ids):
        result = {mode: dict(components=[], evidence=[primary]) for mode in MODES}
        for cid, mode in ids:
            result[mode]["components"].append(cid)
        return result

    artifacts = [dict(file=reference(name, row["sha256"]),
                      coverage=coverage(staged_components.get(name, [])))
                 for name, row in staged.items()]
    artifacts.extend(dict(file=reference(str(path), row["sha256"]),
                          coverage=coverage([external_components[path]]))
                     for path, row in external.items())
    manifest = dict(schema=1, artifacts=artifacts, components=components)

    # Preflight with the exact validator used by release-audit.py: an unreviewed
    # file or a broken reference must fail here, not inside the release gate.
    records = []
    for name, row in staged.items():
        absolute = str((stage.resolve() / Path(name)).absolute())
        records.append(dict(kind="staged-file", path=absolute, **row))
        records.append(dict(kind="file", path=absolute, **row))
        if name in staged_components:
            records.append(dict(kind="native-file", path=absolute, **row))
    for path, row in external.items():
        absolute = str(path)
        records.append(dict(kind="native-file", path=absolute, **row))
        records.append(dict(kind="file", path=absolute, **row))
    def absolute(value):
        # The audit binds stage-relative references to its own absolute paths
        # before validation; mirror that binding for the local preflight.
        if isinstance(value, list):
            return [absolute(item) for item in value]
        if isinstance(value, dict):
            if set(value) in ({"path"}, {"path", "sha256"}) and not value["path"].startswith("/"):
                return dict(value, path=str((stage.resolve() / value["path"]).absolute()))
            return {key: absolute(item) for key, item in value.items()}
        return value

    try:
        validate_closure(absolute(manifest), records, max_files, max_components)
    except (ValueError, KeyError, TypeError) as error:
        raise ValueError("producer closure self-check failed: " + str(error)) from error

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if args.report:
        report = Path(args.report)
        require(not report.resolve().is_relative_to(stage.resolve()),
                "review report must be written outside the release stage")
        report.parent.mkdir(parents=True, exist_ok=True)
        report.write_text(json.dumps(dict(
            scope=mapping["scope"], platform=args.platform,
            mapping={"path": str(mapping_path.resolve().relative_to(ROOT)),
                     "sha256": sha256(mapping_path)},
            pins={name: {"path": location, "sha256": sha256(ROOT / location)}
                  for name, location in mapping["pins"].items()},
            staged_files=len(staged), external_files=len(external),
            components=len(components)), indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(dict(status="generated", platform=args.platform,
                          staged_files=len(staged), external_files=len(external),
                          components=len(components),
                          output=str(output))))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("generate",))
    parser.add_argument("--platform", choices=("macos", "linux"), required=True)
    parser.add_argument("--stage", type=Path, required=True)
    parser.add_argument("--mapping", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args(argv)
    try:
        build(args)
    except (OSError, ValueError, TypeError, KeyError, RecursionError) as error:
        print("POSIX native evidence refused: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
