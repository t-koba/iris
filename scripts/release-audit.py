#!/usr/bin/env python3
"""Inventory staged release bytes and runner runtime evidence, not compliance approval."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import threading


class Audit:
    def __init__(self, args):
        self.args = args
        self.records = []
        self.used = 0
        self.omitted = 0

    def clean(self, text):
        # Never dump the environment, credentials, signing settings or personal paths.
        # Outside GitHub Actions the workspace/home env vars are absent, so also
        # normalize the local workspace root and home directory unconditionally.
        # Windows reports may embed the same path as `C:\\work\\repo` and as
        # `file:///C:/work/repo`; both slash styles and file-URL forms are
        # normalized so no workspace/home path leaks outside Actions.
        bases = []
        for key, replacement in (("GITHUB_WORKSPACE", "$WORKSPACE"),
                                 ("RUNNER_TEMP", "$TEMP"), ("HOME", "$HOME"),
                                 ("USERPROFILE", "$HOME")):
            value = os.environ.get(key)
            if value:
                bases.append((value, replacement))
        try:
            workspace = str(Path(__file__).resolve().parent.parent)
            if workspace:
                bases.append((workspace, "$WORKSPACE"))
        except Exception:
            pass
        try:
            cwd = str(Path.cwd().resolve())
            if cwd:
                bases.append((cwd, "$WORKSPACE"))
        except Exception:
            pass
        try:
            home = str(Path.home())
            if home:
                bases.append((home, "$HOME"))
        except Exception:
            pass
        expanded = set()
        for value, replacement in bases:
            if not value:
                continue
            expanded.add((value, replacement))
            forward = value.replace("\\", "/")
            backward = value.replace("/", "\\")
            if forward != value:
                expanded.add((forward, replacement))
            if backward != value:
                expanded.add((backward, replacement))
            if forward:
                expanded.add(("file://" + forward, replacement))
                expanded.add(("file:///" + forward.lstrip("/"), replacement))
        for value, replacement in sorted(expanded, key=lambda item: -len(item[0])):
            if value:
                text = text.replace(value, replacement)
        return text

    def add(self, kind, **data):
        def scrub(value):
            if isinstance(value, str):
                return self.clean(value)
            if isinstance(value, list):
                return [scrub(item) for item in value]
            if isinstance(value, dict):
                return {key: scrub(item) for key, item in value.items()}
            return value
        record = scrub(dict(kind=kind, **data))
        size = len(json.dumps(record, ensure_ascii=True).encode()) + 2
        if self.used + size > self.args.max_report_bytes:
            self.omitted += 1
        else:
            self.records.append(record)
            self.used += size
            return True
        return False

    @staticmethod
    def sha256_file(path):
        digest = hashlib.sha256()
        with Path(path).open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()

    def pdfium_attestation(self, docs, library):
        result = dict(attestation_verified=False, attestation_status="unknown",
                      asset_sha256=None, library_sha256=None,
                      attestation_pin_sha256=None,
                      note="staged PDFium provenance is missing or invalid")
        provenance = Path(docs) / "PROVENANCE.json"
        try:
            with provenance.open("rb") as stream:
                raw = stream.read(self.args.max_command_bytes + 1)
            if len(raw) > self.args.max_command_bytes:
                return result
            record = json.loads(raw)
            if not isinstance(record, dict) or record.get("schema") != 1:
                return result
            archive = record.get("archive")
            runtime = record.get("runtime")
            attestation = record.get("attestation")
            if not all(isinstance(item, dict) for item in (archive, runtime, attestation)):
                return result
            asset_sha256 = archive.get("sha256")
            runtime_sha256 = runtime.get("sha256")
            attestation_pin_sha256 = attestation.get("pin_sha256")
            if (archive.get("asset") not in ("pdfium-linux-x64.tgz", "pdfium-linux-arm64.tgz")
                    or type(archive.get("bytes")) is not int or archive["bytes"] < 1
                    or not isinstance(asset_sha256, str)
                    or not re.fullmatch(r"[0-9a-f]{64}", asset_sha256)
                    or attestation.get("subject_sha256") != asset_sha256
                    or not isinstance(attestation_pin_sha256, str)
                    or not re.fullmatch(r"[0-9a-f]{64}", attestation_pin_sha256)
                    or runtime.get("path") != "usr/lib/iris/libpdfium.so"
                    or not isinstance(runtime_sha256, str)
                    or not re.fullmatch(r"[0-9a-f]{64}", runtime_sha256)):
                return result
            actual_library_sha256 = self.sha256_file(library)
            if actual_library_sha256 != runtime_sha256:
                return result
        except (OSError, UnicodeDecodeError, json.JSONDecodeError, TypeError, ValueError):
            return result

        status = attestation.get("status")
        if status not in ("verified", "not_attempted"):
            return result
        result.update(attestation_status=status, asset_sha256=asset_sha256,
                      library_sha256=actual_library_sha256,
                      attestation_pin_sha256=attestation_pin_sha256)
        if status == "verified":
            result.update(attestation_verified=True,
                          note="staged provenance records successful verification for this exact archive digest")
        else:
            result.update(note="staged provenance records that upstream attestation verification was not attempted")
        return result

    def command(self, *argv):
        # Temp output is deleted here, including on timeout; retain only bounded text.
        with tempfile.TemporaryFile() as output:
            try:
                result = subprocess.run(argv, stdout=output, stderr=subprocess.STDOUT,
                                        timeout=self.args.command_seconds, check=False)
                status = result.returncode
            except (OSError, subprocess.TimeoutExpired) as error:
                status = type(error).__name__
            length = output.tell()
            output.seek(0)
            text = output.read(self.args.max_command_bytes).decode("utf-8", "replace")
        self.add("command", argv=list(argv), status=status, text=text,
                 truncated=length > self.args.max_command_bytes)
        return text if status == 0 else ""

    def _bounded_cargo(self, argv, record_kind):
        # Drain both pipes concurrently, retaining at most the configured bytes each.
        # An overrun kills the producer rather than filling RAM or a temporary file.
        # The same metadata byte limit bounds both the catalog and the tree output.
        limit = self.args.max_metadata_bytes
        chunks = [b"", b""]
        truncated = [False, False]
        try:
            with subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE) as process:
                def read(index, stream):
                    chunks[index] = stream.read(limit + 1)
                    truncated[index] = len(chunks[index]) > limit
                    chunks[index] = chunks[index][:limit]
                    if truncated[index]:
                        process.kill()
                    stream.close()

                readers = [threading.Thread(target=read, args=(i, stream))
                           for i, stream in enumerate((process.stdout, process.stderr))]
                for reader in readers:
                    reader.start()
                try:
                    status = process.wait(timeout=self.args.command_seconds)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                    status = "TimeoutExpired"
                for reader in readers:
                    reader.join()
        except OSError as error:
            status = type(error).__name__
        self.add(record_kind, argv=argv, status=status,
                 truncated=any(truncated), stdout_bytes=len(chunks[0]),
                 stderr=chunks[1][:self.args.max_command_bytes].decode("utf-8", "replace"),
                 stderr_truncated=truncated[1] or len(chunks[1]) > self.args.max_command_bytes)
        if status != 0 or any(truncated):
            raise ValueError(record_kind + " failed, timed out or exceeded its byte limit")
        return chunks[0]

    def cargo_metadata(self, argv):
        return json.loads(self._bounded_cargo(argv, "cargo-metadata-command"))

    def cargo_tree(self, argv):
        return self._bounded_cargo(argv, "cargo-tree-command").decode("utf-8", "replace")

    def cargo_graph(self, rustc):
        # Product graph and enabled features come from a package-rooted
        # `cargo tree -p iris-ui-shell --target <host>` run. `cargo metadata`
        # is only the package/license catalog.
        # `cargo metadata` alone resolves the whole workspace, so its per-node
        # feature lists overstate the packaged product (workspace-wide
        # unification leaks dev/bench features such as image/avif,
        # tokio/test-util or iris-thumb-preview/test-fixtures). Its
        # `resolve.nodes[*].deps[*].dep_kinds` also keeps every target
        # declaration, so those predicates are never expanded or serialized.
        flags = (["--no-default-features", "--features", "minimal"]
                 if self.args.platform == "windows" else [])
        context = dict(variant="minimal" if flags else "product", cargo_flags=flags,
                       target=None, root=None, truncated=False)
        try:
            hosts = re.findall(r"^host: (\S+)$", rustc, re.M)
            if len(hosts) != 1:
                raise ValueError("rustc host is absent or ambiguous")
            context["target"] = hosts[0]
            # Relative manifest path keeps the recorded argv free of absolute
            # local paths; the tree invocation needs no manifest path at all.
            manifest = Path("crates/ui-shell/Cargo.toml")
            metadata_argv = ["cargo", "metadata", "--format-version", "1", "--locked", "--offline",
                    "--filter-platform", hosts[0], "--manifest-path", str(manifest), *flags]
            tree_argv = ["cargo", "tree", "--color", "never", "-p", "iris-ui-shell", "--target", hosts[0],
                    "--offline", "--locked", "--prefix", "indent", "--charset", "ascii",
                    "--format", "{p}|{f}", "-e", "normal,build", *flags]
            catalog_error = None
            try:
                metadata = self.cargo_metadata(metadata_argv)
            except (OSError, ValueError) as error:
                metadata = None
                catalog_error = str(error)
            tree_text = self.cargo_tree(tree_argv)
            packages = ({p["id"]: p for p in metadata["packages"]}
                        if metadata is not None else {})
            by_key = {}
            for pid, pkg in packages.items():
                key = (pkg["name"], pkg["version"], pkg.get("source") is None)
                by_key.setdefault(key, []).append(pid)
            order = []
            tree_features = {}
            raw_edges = []
            stack = []
            stack_build = []
            seen = set()
            root = None
            for raw_line in tree_text.splitlines():
                if not raw_line.strip():
                    continue
                match = re.match(r"^((?:\|   |    )*(?:\|-- |\`-- )?)(.*)$", raw_line)
                if not match:
                    raise ValueError("unexpected cargo tree line")
                prefix, content = match.group(1), match.group(2)
                if content == "[build-dependencies]":
                    if len(prefix) % 4 != 0:
                        raise ValueError("unexpected build header indent")
                    parent_depth = len(prefix) // 4
                    if parent_depth >= len(stack):
                        raise ValueError("build header without parent")
                    if prefix.endswith("|-- ") or prefix.endswith("`-- "):
                        raise ValueError("unexpected build header branch")
                    if stack_build[parent_depth]:
                        raise ValueError("duplicate build header")
                    stack_build[parent_depth] = True
                    continue
                if content.startswith("[") and content.endswith("]"):
                    raise ValueError("unexpected cargo tree header")
                if len(prefix) % 4 != 0:
                    raise ValueError("unexpected cargo tree indent")
                depth = len(prefix) // 4
                if depth == 0:
                    if prefix != "":
                        raise ValueError("unexpected tree root prefix")
                elif not (prefix.endswith("|-- ") or prefix.endswith("`-- ")):
                    raise ValueError("unexpected cargo tree branch")
                rest = content
                if rest.endswith(" (*)"):
                    rest = rest[:-4]
                if "|" not in rest:
                    raise ValueError("unexpected cargo tree format")
                p_raw, f_raw = rest.split("|", 1)
                p_raw = p_raw.strip()
                f_raw = f_raw.strip()
                parts = p_raw.split()
                if len(parts) < 2:
                    raise ValueError("unexpected package display")
                name = parts[0]
                version_token = parts[1]
                version = version_token[1:] if version_token.startswith("v") else version_token
                hint = re.sub(r"\s+\(proc-macro\)$", "", p_raw)
                source_match = re.search(r"\((.+)\)$", hint)
                location = source_match.group(1) if source_match else ""
                is_path = (location.startswith(("/", "\\", "file://", "./", "../")) or
                           bool(re.match(r"^[A-Za-z]:[\\/]", location)))
                if metadata is None:
                    # These are tree display identities, not invented metadata IDs.
                    # In particular, a bare registry display does not identify its
                    # registry URL. Retain only the source text Cargo actually gave us.
                    display = re.fullmatch(r"([A-Za-z0-9_-]+) v(\S+?)(?: \((.+)\))?", hint)
                    if not display or display.groups() != (name, version, location or None):
                        raise ValueError("ambiguous tree package identity")
                    pid = "tree:" + hint
                    proc_macro = p_raw.endswith(" (proc-macro)")
                    package = dict(id=pid, name=name, version=version,
                                   source=location or None,
                                   source_kind="path" if is_path else "tree-location" if location else "unavailable",
                                   targets=[dict(kind=["proc-macro"])] if proc_macro else [])
                    if pid in packages and packages[pid] != package:
                        raise ValueError("ambiguous tree package identity")
                    packages[pid] = package
                else:
                    candidates = by_key.get((name, version, is_path))
                    if candidates is None:
                        raise ValueError(f"tree package {name} {version} not in catalog")
                    if len(candidates) != 1:
                        raise ValueError(f"ambiguous catalog entry for {name} {version}")
                    pid = candidates[0]
                feats = [f.strip() for f in f_raw.split(",") if f.strip()] if f_raw else []
                if pid not in seen:
                    seen.add(pid)
                    order.append(pid)
                    tree_features[pid] = feats
                if depth == 0:
                    if stack:
                        raise ValueError("multiple tree roots")
                    stack = [pid]
                    stack_build = [False]
                    root = pid
                    context["root"] = root
                else:
                    if depth > len(stack):
                        raise ValueError("tree depth gap")
                    parent = stack[depth - 1]
                    kind = "build" if stack_build[depth - 1] else "normal"
                    raw_edges.append((parent, pid, kind))
                    stack = stack[:depth] + [pid]
                    stack_build = stack_build[:depth] + [False]
            if root is None:
                raise ValueError("empty cargo tree")
            if packages[root]["name"] != "iris-ui-shell":
                raise ValueError("tree root is not iris-ui-shell")
            # Bound the rooted closure; tree order is depth-first first occurrence.
            selected = set()
            for pid in order:
                if len(selected) >= self.args.max_cargo_packages:
                    break
                selected.add(pid)
            context["truncated"] = len(seen) > len(selected)
            # The tree already applied target filtering and excluded dev-only
            # and unrelated packages via -p and -e. One record per package
            # relationship; the kind comes from the tree's build groups, never
            # from unfiltered metadata target predicates. A package edge in
            # both groups preserves both kinds and contexts.
            edge_kinds = {}
            for src, dst, kind in raw_edges:
                if src not in selected or dst not in selected:
                    continue
                edge_kinds.setdefault((src, dst), set()).add(kind)
            edges = []
            for (src, dst), kinds in edge_kinds.items():
                proc_macro = any("proc-macro" in t.get("kind", []) for t in packages[dst].get("targets", []))
                name = packages[dst].get("name", "").replace("-", "_")
                dependency_kinds = sorted(kinds)
                dependency_kind = dependency_kinds[0] if len(dependency_kinds) == 1 else "ambiguous"
                edges.append(dict(source=src, destination=dst, name=name,
                                  dependency_kind=dependency_kind, dependency_kinds=dependency_kinds,
                                  proc_macro=proc_macro))
            # A normal dependency of a build tool is still build-time, not runtime.
            contexts = {identity: set() for identity in selected}
            contexts[root].add("runtime")
            outgoing = {identity: [] for identity in selected}
            for edge in edges:
                outgoing[edge["source"]].append(edge)
            pending = [(root, "runtime")]
            for identity, usage in pending:
                for edge in outgoing[identity]:
                    roles = ({"proc-macro"} if edge["proc_macro"] else set())
                    if not edge["proc_macro"]:
                        if "normal" in edge["dependency_kinds"]:
                            roles.add("runtime" if usage == "runtime" else "build")
                        if "build" in edge["dependency_kinds"]:
                            roles.add("build")
                    dest = edge["destination"]
                    for role in roles:
                        if role not in contexts[dest]:
                            contexts[dest].add(role)
                            pending.append((dest, role))
            projected = []
            for pid in order:
                if pid not in selected:
                    continue
                package = packages[pid]
                projected.append(dict(
                    id=pid, name=package["name"], version=package["version"],
                    source=package.get("source"),
                    source_kind="path" if package.get("source") is None else "registry-or-git",
                    features=tree_features[pid], contexts=sorted(contexts[pid]),
                    target_kinds=sorted({k for t in package["targets"] for k in t.get("kind", [])}),
                    license=package.get("license"), license_file=package.get("license_file"),
                    missing_metadata=[k for k in ("license", "license_file") if not package.get(k)]))
                if metadata is None:
                    item = projected[-1]
                    item.update(status="partial", tree_identity=pid.removeprefix("tree:"),
                                source_kind=package["source_kind"])
                    item["missing_metadata"].append("target_kinds")
                    if not package["source"]:
                        item["missing_metadata"].append("source")
            if catalog_error is not None:
                context["metadata_error"] = catalog_error
            if self.add("cargo-graph", **context, status="partial" if context["truncated"] or metadata is None else "captured",
                        packages=projected, edges=edges,
                        note=("Package catalog unavailable: tree display identities and source locations only, not metadata package IDs or registry URLs; licenses and non-proc-macro target kinds are unavailable. " if metadata is None else "") + "Cargo tree features for the selected target/variant and declared license metadata only; path source is not a registry. Edges are tree package relationships once each; dependency_kinds retains normal/build group membership and dependency_kind is ambiguous when both apply. Unfiltered metadata target predicates are never serialized. Dev-only and unreachable packages are excluded via -p and -e. Runtime/build contexts propagate through normal dependencies, preserving both for dual-kind edges; proc-macro dependencies run at build time. No legal conclusion or native-code closure is inferred."):
                return
            context["truncated"] = True
            reason = "rooted graph omitted: serialized report byte budget exceeded"
        except (OSError, ValueError, KeyError, TypeError) as error:
            reason = "cargo metadata/tree absent or invalid: " + str(error)
        self.add("cargo-graph", **context, status="absent", reason=reason,
                 packages=[], edges=[])

    def file(self, path, text=False):
        path = Path(path)
        if not path.is_file():
            self.add("missing", path=str(path), reason="file absent (or system shared cache)")
            return
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        data = dict(path=str(path), bytes=path.stat().st_size, sha256=digest.hexdigest())
        if text:
            with path.open("rb") as stream:
                data["text"] = stream.read(self.args.max_command_bytes).decode("utf-8", "replace")
            data["truncated"] = data["bytes"] > self.args.max_command_bytes
        self.add("file", **data)

    def ffmpeg(self, path):
        if re.match(r"lib(?:avcodec|avformat|avutil|avdevice|avfilter|swscale|swresample)[.-]", path.name):
            # Probe the exact library, never a standalone ffmpeg or the Rust binding.
            self.command(sys.executable, str(Path(__file__).resolve()),
                         "probe-library", str(path.resolve()))

    def linux(self, roots):
        self.file("/etc/os-release", text=True)
        self.command("apt-cache", "policy")
        packages = set()
        for path in self.closure(roots, "linux"):
            # Debian's /usr merge means the package database may use either spelling.
            spelling = str(path)
            owners = self.command("dpkg-query", "-S", spelling)
            if not owners and spelling.startswith("/usr/"):
                owners = self.command("dpkg-query", "-S", spelling[4:])
            found = False
            for line in owners.splitlines():
                if ": /" in line:
                    packages.add(line.split(": /", 1)[0])
                    found = True
            if not found:
                self.add("missing", path=spelling, reason="no Debian package owner; see bundled PDFium evidence if applicable")
        for package in sorted(packages):
            self.command("dpkg-query", "-W", "-f=${binary:Package}\t${Version}\t${Architecture}\t${source:Package}\t${source:Version}\n", package)
            self.command("apt-cache", "policy", package)
            self.file(Path("/usr/share/doc") / package.split(":")[0] / "copyright", text=True)
        self.add("scope", note="Debian policy shows configured origins/candidate and installed version, not proof of the historical download origin. Copyright is package evidence, not a compatibility verdict. Dynamically loaded plugins outside these roots remain unknown.")

    def closure(self, roots, platform):
        queue = list(roots)
        seen = set()
        while queue and len(seen) < self.args.max_native_files:
            path = queue.pop(0).resolve()
            if path in seen:
                continue
            seen.add(path)
            self.file(path)
            if not path.is_file():
                continue
            self.ffmpeg(path)
            if platform == "linux":
                self.command("readelf", "-d", str(path))
                listing = self.command("ldd", str(path))
                if not listing:
                    self.add("missing", path=str(path), reason="ldd dependency evidence unavailable")
                deps = re.findall(r"(?:=>\s+|^\s*)(/\S+)\s+\(", listing, re.M)
                for line in listing.splitlines():
                    if "not found" in line:
                        self.add("missing", parent=str(path), dependency=line.strip())
                queue.extend(Path(dep) for dep in deps)
            else:
                listing = self.command("otool", "-L", str(path))
                loads = self.command("otool", "-l", str(path))
                if not listing:
                    self.add("missing", path=str(path), reason="otool dependency evidence unavailable")
                rpaths = re.findall(r"cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset", loads)
                for line in listing.splitlines()[1:]:
                    dep = line.strip().split(" (", 1)[0]
                    resolved = self.mac_dependency(dep, path, roots[0], rpaths)
                    if resolved and str(resolved).startswith(("/System/", "/usr/lib/")):
                        self.add("system-dependency", parent=str(path), dependency=dep,
                                 resolved=str(resolved), note="provided by macOS; not shipped in the app bundle")
                    elif resolved:
                        queue.append(resolved)
                    else:
                        self.add("missing", parent=str(path), dependency=dep,
                                 reason="unresolved install name; inherited rpath/shared cache resolution not assumed")
            yield path
        if queue:
            self.add("missing", reason="native closure scan limit reached", pending=len(queue))

    @staticmethod
    def mac_dependency(dep, loader, executable, rpaths):
        def expand(value):
            return value.replace("@loader_path", str(loader.parent)).replace(
                "@executable_path", str(executable.parent))
        if dep.startswith("@rpath/"):
            candidates = [Path(expand(p)) / dep[len("@rpath/"):] for p in rpaths]
            return next((p for p in candidates if p.is_file()), None)
        if dep.startswith(("/", "@loader_path/", "@executable_path/")):
            return Path(expand(dep))
        return None

    def macos(self, roots):
        closure = list(self.closure(roots, "macos"))
        cellar = self.command("brew", "--cellar").strip()
        reported = set()
        for path in closure:
            if not path.name.endswith(".dylib"):
                continue
            candidates = sorted(Path(cellar).glob("*/*/lib/" + path.name)) if cellar else []
            for source in candidates:
                self.file(source)
                keg = source.parent.parent
                formula = keg.parent.name
                self.add("homebrew", staged=str(path), candidate=str(source),
                         formula=formula, installed_version=keg.name,
                         relationship="basename candidate, NOT byte-equivalence: packaging rewrites install names and signs dylibs")
                if keg not in reported:
                    reported.add(keg)
                    self.command("brew", "info", "--json=v2", formula)
                    self.file(keg / "INSTALL_RECEIPT.json", text=True)
                    self.file(keg / ".brew" / (formula + ".rb"), text=True)
            self.add("provenance-match", staged=str(path), candidates=[str(p) for p in candidates],
                     status="ambiguous" if len(candidates) > 1 else "candidate only" if candidates else "missing")
        self.add("scope", note="otool describes exact staged load commands. System shared-cache libraries may have no readable bytes. Homebrew metadata/receipts are available provenance candidates, not proof of source patches or license compatibility. dlopen-only runtimes not staged here remain unknown.")

    def windows(self, executable):
        self.file(executable)
        payload = sorted(p for p in executable.parent.rglob("*") if p.is_file())
        self.add("windows-payload", files=[str(p) for p in payload],
                 ffmpeg_pdfium_files=[str(p) for p in payload if re.search(
                     r"(?:avcodec|avformat|avutil|pdfium)", p.name, re.I)])
        for path in payload:
            if path != executable:
                self.file(path)
        self.add("build", variant="minimal", cargo_flags=["--no-default-features", "--features", "minimal"],
                 native_preview="FFmpeg and PDFium are not selected by this release build; no video/PDF preview support is asserted.")
        tool = shutil.which("llvm-readobj")
        if not tool:
            candidate = Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "LLVM/bin/llvm-readobj.exe"
            if candidate.is_file():
                tool = str(candidate)
        if tool:
            self.command(tool, "--coff-imports", str(executable))
        else:
            installer = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Microsoft Visual Studio/Installer/vswhere.exe"
            vs = self.command(str(installer), "-latest", "-products", "*", "-property", "installationPath").strip()
            tools = sorted(Path(vs).glob("VC/Tools/MSVC/*/bin/Hostx64/x64/dumpbin.exe")) if vs else []
            if tools:
                self.command(str(tools[-1]), "/NOLOGO", "/IMPORTS", str(executable))
            else:
                self.add("missing", reason="no llvm-readobj or Visual Studio dumpbin import inspector")
        self.add("scope", note="Imports are direct PE imports, not proof of the runtime closure or static library licenses. Absence of FFmpeg/PDFium selection follows explicit minimal build flags, not executable naming.")


def probe_library(path):
    import ctypes
    try:
        lib = ctypes.CDLL(path)
        result = {"library": path}
        for name in ("av_version_info", "avcodec_version", "avcodec_configuration", "avcodec_license",
                     "avformat_version", "avformat_configuration", "avformat_license",
                     "avdevice_version", "avdevice_configuration", "avdevice_license",
                     "avfilter_version", "avfilter_configuration", "avfilter_license",
                     "avutil_version", "avutil_configuration", "avutil_license",
                     "swscale_version", "swscale_configuration", "swscale_license",
                     "swresample_version", "swresample_configuration", "swresample_license"):
            try:
                function = getattr(lib, name)
            except AttributeError:
                continue
            function.restype = ctypes.c_uint if name.endswith("_version") else ctypes.c_char_p
            value = function()
            result[name] = value.decode("utf-8", "replace") if isinstance(value, bytes) else value
        print(json.dumps(result))
    except OSError:
        print("MISSING: exact-library load failed; no substitute executable was queried")
        return 1
    return 0


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "probe-library":
        return probe_library(sys.argv[2])
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("platform", choices=("macos", "linux", "windows"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--max-report-bytes", type=int,
                        default=os.environ.get("IRIS_RELEASE_AUDIT_MAX_REPORT_BYTES", "2097152"))
    parser.add_argument("--max-command-bytes", type=int,
                        default=os.environ.get("IRIS_RELEASE_AUDIT_MAX_COMMAND_BYTES", "32768"))
    parser.add_argument("--max-metadata-bytes", type=int,
                        default=os.environ.get("IRIS_RELEASE_AUDIT_MAX_METADATA_BYTES", "8388608"))
    parser.add_argument("--max-cargo-packages", type=int,
                        default=os.environ.get("IRIS_RELEASE_AUDIT_MAX_CARGO_PACKAGES", "1024"))
    parser.add_argument("--max-native-files", type=int,
                        default=os.environ.get("IRIS_RELEASE_AUDIT_MAX_NATIVE_FILES", "256"))
    parser.add_argument("--command-seconds", type=int,
                        default=os.environ.get("IRIS_RELEASE_AUDIT_COMMAND_SECONDS", "30"))
    args = parser.parse_args()
    if min(args.max_report_bytes, args.max_command_bytes, args.max_native_files, args.command_seconds,
           args.max_metadata_bytes, args.max_cargo_packages) < 1:
        parser.error("all bounds must be positive")
    limits = vars(args).copy()
    del limits["output"]
    minimum_report = dict(schema=1, platform=args.platform, limits=limits,
                          omitted_records=0, records=[])
    if len((json.dumps(minimum_report, ensure_ascii=True) + "\n").encode()) > args.max_report_bytes:
        parser.error("report budget is too small for its metadata")
    audit = Audit(args)
    audit.add("scope", platform=args.platform, note="Evidence inventory only; missing/truncated/ambiguous evidence is NOT approval. No Iris project license or compatibility is asserted. Runtime identity is never inferred from ffmpeg-next or a standalone ffmpeg executable.")
    audit.command("git", "rev-parse", "HEAD")
    rustc = audit.command("rustc", "--version", "--verbose")
    audit.cargo_graph(rustc)
    audit.file("Cargo.lock")
    audit.file("Cargo.toml", text=True)
    audit.file("crates/ui-shell/Cargo.toml", text=True)
    directory = Path("dist") / args.platform
    for path in sorted(directory.iterdir()) if directory.exists() else []:
        if path.is_file():
            audit.file(path)
    if args.platform == "linux":
        app = directory / "Iris.AppDir"
        audit.add("build", variant="product", features="Cargo defaults", opt_level="s")
        docs = app / "usr/share/doc/iris/pdfium"
        audit.add("provenance-check", subject="PDFium",
                  **audit.pdfium_attestation(docs, app / "usr/lib/iris/libpdfium.so"))
        audit.linux([app / "usr/bin/Iris", app / "usr/lib/iris/libpdfium.so"])
        if not docs.is_dir():
            audit.add("missing", path=str(docs), reason="PDFium provenance/license directory absent")
        else:
            for path in sorted(docs.rglob("*")):
                if path.is_file():
                    audit.file(path, text=True)
    elif args.platform == "macos":
        contents = directory / "Iris.app/Contents"
        audit.add("build", variant="product", features="Cargo defaults")
        audit.macos([contents / "MacOS/Iris", *sorted((contents / "Frameworks").glob("*.dylib"))])
    else:
        audit.windows(directory / "Iris/Iris.exe")
    while True:
        report = dict(schema=1, platform=args.platform, limits=limits,
                      omitted_records=audit.omitted, records=audit.records)
        encoded = json.dumps(report, ensure_ascii=True) + "\n"
        if len(encoded.encode()) <= args.max_report_bytes:
            break
        if not audit.records:
            parser.error("report exceeds its configured size budget")
        audit.records.pop()
        audit.omitted += 1
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(encoded, encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
