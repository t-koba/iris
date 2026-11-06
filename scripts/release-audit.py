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


class Audit:
    def __init__(self, args):
        self.args = args
        self.records = []
        self.used = 0
        self.omitted = 0

    def clean(self, text):
        # Never dump the environment, credentials, signing settings or personal paths.
        for key, replacement in (("GITHUB_WORKSPACE", "$WORKSPACE"),
                                 ("RUNNER_TEMP", "$TEMP"), ("HOME", "$HOME"),
                                 ("USERPROFILE", "$HOME")):
            value = os.environ.get(key)
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
    parser.add_argument("--max-native-files", type=int,
                        default=os.environ.get("IRIS_RELEASE_AUDIT_MAX_NATIVE_FILES", "256"))
    parser.add_argument("--command-seconds", type=int,
                        default=os.environ.get("IRIS_RELEASE_AUDIT_COMMAND_SECONDS", "30"))
    args = parser.parse_args()
    if min(args.max_report_bytes, args.max_command_bytes, args.max_native_files, args.command_seconds) < 1:
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
    audit.command("rustc", "--version", "--verbose")
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
        audit.add("provenance-check", subject="PDFium",
                  attestation_verified=os.environ.get("IRIS_PDFIUM_ATTESTATION_VERIFIED") == "true",
                  note="A manual package audit verifies the pinned asset size/digest but does not receive a GitHub token for attestation verification; tag releases verify the upstream attestation in preflight.")
        audit.linux([app / "usr/bin/Iris", app / "usr/lib/iris/libpdfium.so"])
        docs = app / "usr/share/doc/iris/pdfium"
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
