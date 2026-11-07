#!/usr/bin/env python3
"""Inspect the actual ELF runtime, not Cargo metadata or a system ffmpeg CLI."""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
POLICY = json.loads((ROOT / "packaging/linux/ffmpeg.json").read_text())


def run(*args):
    env = {k: v for k, v in os.environ.items() if not k.startswith("LD_")}
    return subprocess.check_output(args, text=True, env=env).strip()


def require(ok, message):
    if not ok:
        raise ValueError(message)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def signature(text):
    valid = [line.split()[2:] for line in text.splitlines()
             if line.startswith("[GNUPG:] VALIDSIG ")]
    # A signing subkey is accepted only when GnuPG binds it to the required primary.
    require(len(valid) == 1 and len(valid[0]) in (9, 10) and (valid[0][0] == POLICY["fingerprint"] or
                                valid[0][-1] == POLICY["fingerprint"]),
            "signature is not from the pinned FFmpeg signing key")
    require(not re.search(r'\[GNUPG:\] (BADSIG|ERRSIG|REVKEYSIG|EXPKEYSIG|EXPSIG) ', text),
            "invalid, expired or revoked signature")


def config(text):
    for name, expected in {"GPL": 0, "VERSION3": 0, "NONFREE": 0, "SHARED": 1, "STATIC": 0}.items():
        require(re.search(rf'^#define CONFIG_{name} {expected}$', text, re.M),
                f"unacceptable CONFIG_{name}")


def codecs(text):
    enabled = re.findall(r'^#define CONFIG_(\w+_(?:DECODER|ENCODER)) 1$', text, re.M)
    require(set(POLICY["required_decoders"]) <= set(enabled), "missing default built-in decoder")
    return enabled


def inventory(directory):
    result = {}
    for path in sorted(directory.glob("*.so*")):
        if path.name == "libpdfium.so":
            continue  # Independently pinned dlopen runtime, checked by PDFium smoke/shlibdeps.
        if path.is_symlink():
            target = os.readlink(path)
            require(Path(target).name == target and (directory / target).is_file(),
                    f"unsafe shared library symlink: {path}")
            result[path.name] = {"target": target}
        else:
            require(path.is_file(), f"not a shared library: {path}")
            result[path.name] = {"sha256": digest(path)}
    require(result, "no shared libraries")
    return result


def soname(path):
    matches = re.findall(r'\(SONAME\).*\[(.*?)\]', run("readelf", "-d", str(path)))
    require(len(matches) == 1, f"missing/ambiguous SONAME: {path}")
    return matches[0]


def resolution(binary):
    text = run("ldd", str(binary))
    require("not found" not in text, f"unresolved dependency: {binary}\n{text}")
    require("=>" in text, f"no dynamic library resolution: {binary}")
    resolved = dict(re.findall(r'^\s*(\S+) => (/\S+) \(', text, re.M))
    # ldd prints the ELF loader without the usual SONAME => prefix.
    for loader in re.findall(r'^\s*(/\S+) \(', text, re.M):
        resolved[Path(loader).name] = loader
    return resolved


def system_dependency(path):
    resolved = Path(path).resolve()
    try:
        owner = run("dpkg-query", "-S", str(resolved))
    except subprocess.CalledProcessError:
        # Debian's pre-usrmerge package database may retain the /lib alias.
        owner = run("dpkg-query", "-S", str(resolved).removeprefix("/usr"))
    owners = [line.rsplit(": ", 1)[0] for line in owner.splitlines()]
    require(len(owners) == 1, f"ambiguous system library owner: {path}")
    package = owners[0]
    base = package.split(":")[0]
    require(base in POLICY["system_packages"], f"unreviewed system library package: {package}")
    identity = run("dpkg-query", "-W", "-f=${binary:Package}\t${Version}\t${source:Package}\t${source:Version}", package)
    require(len(identity.split("\t")) == 4 and all(identity.split("\t")),
            f"missing system package source identity: {package}")
    copyright_file = Path("/usr/share/doc") / base / "copyright"
    require(copyright_file.is_file(), f"missing system copyright: {package}")
    return dict(package_identity=identity.split("\t"), sha256=digest(resolved),
                license_reference=POLICY["system_packages"][base],
                copyright_sha256=digest(copyright_file))


def runtime(libdir):
    """Inspect each actual DSO's ABI, compiled configuration, license and closure."""
    records = []
    system = {}
    for name in POLICY["libraries"]:
        link = libdir / f"lib{name}.so"
        require(link.is_file(), f"missing {link}")
        so = soname(link)
        require(re.fullmatch(rf'lib{name}\.so\.\d+', so), f"unexpected SONAME {so}")
        closure = resolution(link)
        dependencies = {}
        for dependency, path in closure.items():
            if dependency.startswith(("libav", "libswscale", "libswresample")):
                require(Path(path).resolve().parent == libdir.resolve(),
                        f"system FFmpeg dependency: {dependency}")
                dependencies[dependency] = {"bundled": Path(path).name}
            else:
                require(dependency in POLICY["system_sonames"],
                        f"unreviewed FFmpeg dependency: {dependency}")
                if path not in system:
                    system[path] = system_dependency(path)
                dependencies[dependency] = system[path]
        library = ctypes.CDLL(str(link.resolve()))
        configuration = getattr(library, f"{name}_configuration")
        configuration.restype = ctypes.c_char_p
        flags = shlex.split(configuration().decode())
        selected = [flag for flag in flags if not flag.startswith(("--prefix=", "--libdir="))]
        require(selected == POLICY["configure_flags"], f"unexpected {name} build flags: {flags}")
        license_fn = getattr(library, f"{name}_license")
        license_fn.restype = ctypes.c_char_p
        license_text = license_fn().decode()
        require(license_text == "LGPL version 2.1 or later", f"unacceptable {name} license")
        version_fn = getattr(library, f"{name}_version")
        version_fn.restype = ctypes.c_uint
        value = version_fn()
        version = [value >> 16, (value >> 8) & 255, value & 255]
        if name == "avcodec":
            require(version == POLICY["avcodec_version"], f"unexpected avcodec ABI: {version}")
        records.append(dict(library=so, version=version, configuration=flags,
                            license=license_text, dependencies=dependencies))
    actual = {soname(p) for p in libdir.glob("*.so*") if not p.is_symlink() and p.name != "libpdfium.so"}
    require(actual == {r["library"] for r in records}, "unreviewed bundled library")
    return records


def validate(libdir, docs):
    manifest = json.loads((docs / "manifest.json").read_text())
    require(manifest["policy"] == POLICY, "runtime policy differs from current pin")
    require(manifest["signature_verified"] is True, "missing verified signature")
    require(digest(docs / "source.tar.xz") == POLICY["sha256"], "source hash mismatch")
    signature((docs / "signature.status").read_text())
    config((docs / "config.h").read_text())
    require(manifest["enabled_codecs"] == codecs((docs / "config_components.h").read_text()),
            "enabled codec inventory changed")
    require(manifest["documents"] == {p.name: digest(p) for p in docs.iterdir()
                                     if p.name != "manifest.json"}, "documentation changed")
    require(manifest["libraries"] == inventory(libdir), "runtime bytes changed")
    require(manifest["runtime"] == runtime(libdir), "runtime identity changed")
    return manifest


def record(prefix):
    libdir, docs = prefix / "lib", prefix / "share/iris-ffmpeg"
    for path in libdir.glob("*.so*"):
        if not path.is_symlink():
            run("patchelf", "--set-rpath", "$ORIGIN", str(path))
    shutil.copy2(ROOT / "packaging/linux/ffmpeg-README.md", docs / "README.md")
    data = dict(policy=POLICY, signature_verified=True,
                libraries=inventory(libdir), runtime=runtime(libdir),
                enabled_codecs=codecs((docs / "config_components.h").read_text()),
                documents={p.name: digest(p) for p in docs.iterdir()},
                tools={tool: run(tool, "--version").splitlines()[0]
                       for tool in ("cc", "ld", "make", "nasm", "patchelf", "pkg-config", "gpg", "rustc", "cargo")})
    (docs / "manifest.json").write_text(json.dumps(data, indent=2) + "\n")
    validate(libdir, docs)


def stage(prefix, appdir):
    validate(prefix / "lib", prefix / "share/iris-ffmpeg")
    libdir, docs = appdir / "usr/lib/iris", appdir / "usr/share/doc/iris/ffmpeg"
    libdir.mkdir(parents=True, exist_ok=True)
    # The packager owns a fresh AppDir; refuse stale mixed native libraries.
    require(not list(libdir.glob("libav*.so*")) and not list(libdir.glob("libsw*.so*")),
            "stale FFmpeg runtime in AppDir")
    for path in (prefix / "lib").glob("*.so*"):
        if path.is_symlink():
            (libdir / path.name).symlink_to(os.readlink(path))
        else:
            shutil.copy2(path, libdir)
    shutil.copytree(prefix / "share/iris-ffmpeg", docs)
    run("patchelf", "--set-rpath", "$ORIGIN/../lib/iris", str(appdir / "usr/bin/Iris"))
    # PDFium has its own independently verified provenance and dependency check.
    # Restrict runtime validation to the exact FFmpeg set, not the dlopen PDFium.


def check(appdir, output, max_bytes):
    libdir = appdir / "usr/lib/iris"
    docs = appdir / "usr/share/doc/iris/ffmpeg"
    manifest = validate(libdir, docs)
    binary = appdir / "usr/bin/Iris"
    closure = resolution(binary)
    bundled = {}
    for name, path in closure.items():
        if name.startswith(("libav", "libswscale", "libswresample")):
            require(name in manifest["libraries"], f"unreviewed product library: {name}")
            require(Path(path).resolve() == (libdir / name).resolve(),
                    f"Iris resolves system FFmpeg: {name} => {path}")
            bundled[name] = digest(Path(path))
    require(any(n.startswith("libavcodec.so.") for n in bundled), "Iris does not link libavcodec")
    require(any(n.startswith("libavformat.so.") for n in bundled), "Iris does not link libavformat")
    report = dict(source_sha=run("git", "-C", str(ROOT), "rev-parse", "HEAD"),
                  iris_sha256=digest(binary), iris_bytes=binary.stat().st_size,
                  runtime=manifest, resolved_bundled_libraries=bundled,
                  system_product_dependencies=sorted(set(closure) - set(bundled)),
                  system_requirements=(docs.parent / "system-dependencies.txt").read_text().strip())
    data = json.dumps(report, indent=2) + "\n"
    require(len(data.encode()) <= max_bytes, "audit report exceeds configured byte budget")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["signature", "config", "record", "validate", "stage", "check", "shlibs"])
    parser.add_argument("path", type=Path)
    parser.add_argument("destination", type=Path, nargs="?")
    parser.add_argument("--max-report-bytes", type=int, default=int(os.environ.get("IRIS_FFMPEG_MAX_REPORT_BYTES", "262144")))
    args = parser.parse_args()
    if args.command == "signature":
        signature(args.path.read_text())
    elif args.command == "config":
        config(args.path.read_text())
    elif args.command == "record":
        record(args.path)
    elif args.command == "validate":
        validate(args.path / "lib", args.path / "share/iris-ffmpeg")
    elif args.command == "stage":
        stage(args.path, args.destination)
    elif args.command == "check":
        check(args.path, args.destination, args.max_report_bytes)
    else:
        lines = []
        for name in POLICY["libraries"]:
            so = soname(args.path / f"lib{name}.so")
            library, version = so.split(".so.")
            lines.append(f"{library} {version} iris")
        args.destination.write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
