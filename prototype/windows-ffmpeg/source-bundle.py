"""Prototype FFmpeg source material only; not complete dependency source or legal approval."""
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parent
PINS = json.loads((ROOT / "pins.json").read_text(encoding="utf-8-sig"))
COMMIT = PINS["ffmpeg"]["commit"]
BASELINE = PINS["vcpkg"]["baseline"]
ARCHIVE = f"ffmpeg-{COMMIT}.tar"
INPUTS = (
    "pins.json", "vcpkg.json", "vcpkg-configuration.json",
    "triplets/x64-windows-prototype-dynamic.cmake",
    "derive-ffmpeg-overlay.ps1", "build-windows-ffmpeg-prototype.ps1",
    "assert-windows-toolchain-prototype.ps1", "source-bundle.py", "README.md",
)
HELPER = "scripts/cmake/vcpkg_from_git.cmake"


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args])


def record(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return {"bytes": path.stat().st_size, "sha256": digest.hexdigest()}


def patch_series(port):
    # Preserve the entire ordered block, including whitespace and comments.
    blocks = re.findall(r"(?ms)^vcpkg_from_github\(.*?\bPATCHES\s*\n(.*?)^\)", port)
    if len(blocks) != 1:
        raise ValueError("expected one baseline PATCHES block")
    patches = []
    for line in blocks[0].splitlines():
        entry = line.split("#", 1)[0].strip().strip('"')
        if not entry:
            continue
        if not re.fullmatch(r"[A-Za-z0-9_-][A-Za-z0-9_./-]*\.patch", entry) or ".." in entry.split("/"):
            raise ValueError("unsupported patch path")
        patches.append("baseline/ports/ffmpeg/" + entry)
    if not patches or len(set(patches)) != len(patches):
        raise ValueError("empty or duplicate patch series")
    return {"block": blocks[0], "ordered_files": patches}


def put(dest, name, data):
    path = dest / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def create(source, vcpkg, overlay, dest):
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    # git archive reads the commit tree, never the patched checkout. Pin tar
    # permissions too: bytes depend on the pinned commit, not local umask/time.
    subprocess.run(["git", "-C", str(source), "-c", "tar.umask=0022", "archive",
                    "--format=tar", f"--prefix=ffmpeg-{COMMIT}/",
                    "-o", str(dest / ARCHIVE), COMMIT], check=True)
    # Read exact baseline blobs, not checkout line endings or untracked files.
    names = git(vcpkg, "ls-tree", "-r", "--name-only", BASELINE, "--",
                "ports/ffmpeg", HELPER).decode().splitlines()
    if not names or HELPER not in names:
        raise ValueError("missing pinned baseline port/helper")
    # Reject modified tracked build inputs, while allowing unrelated vcpkg state.
    subprocess.run(["git", "-C", str(vcpkg), "diff", "--exit-code", BASELINE,
                    "--", "ports/ffmpeg", HELPER], check=True)
    for name in names:
        put(dest, "baseline/" + name, git(vcpkg, "show", f"{BASELINE}:{name}"))
    for path in sorted(overlay.rglob("*")):
        if path.is_file():
            put(dest, "overlay/ffmpeg/" + path.relative_to(overlay).as_posix(), path.read_bytes())
    for name in INPUTS:
        put(dest, "build-inputs/" + name, (ROOT / name).read_bytes())
    put(dest, "build-inputs/windows-ffmpeg-prototype.yml",
        (ROOT.parent.parent / ".github/workflows/windows-ffmpeg-prototype.yml").read_bytes())
    manifest = {
        "schema": 1, "ffmpeg_commit": COMMIT, "vcpkg_baseline": BASELINE,
        "scope": "FFmpeg upstream tree, retained vcpkg FFmpeg port/patches and prototype recipe only. "
                 "Not complete source for other dependencies, vcpkg tooling or SDKs; no legal conclusion.",
        "patches": patch_series((dest / "baseline/ports/ffmpeg/portfile.cmake").read_bytes().decode()),
        "files": {p.relative_to(dest).as_posix(): record(p)
                  for p in sorted(dest.rglob("*")) if p.is_file()},
    }
    put(dest, "MANIFEST.json", (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode())
    verify(dest, record(dest / "MANIFEST.json"))


def verify(bundle, expected):
    if record(bundle / "MANIFEST.json") != expected:
        raise ValueError("source bundle manifest hash/byte mismatch")
    manifest = json.loads((bundle / "MANIFEST.json").read_text())
    if (manifest["schema"], manifest["ffmpeg_commit"], manifest["vcpkg_baseline"]) != (1, COMMIT, BASELINE):
        raise ValueError("source bundle pin/schema mismatch")
    files = manifest["files"]
    required = {ARCHIVE, "baseline/ports/ffmpeg/portfile.cmake", "baseline/ports/ffmpeg/vcpkg.json",
                "baseline/" + HELPER, "overlay/ffmpeg/portfile.cmake", "overlay/ffmpeg/vcpkg.json",
                "build-inputs/windows-ffmpeg-prototype.yml"} | {"build-inputs/" + n for n in INPUTS}
    if not required <= files.keys():
        raise ValueError("source bundle missing required archive/port/recipe")
    for name, info in files.items():
        path = PurePosixPath(name)
        if path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
            raise ValueError("unsafe source bundle path")
        if record(bundle / name) != info:
            raise ValueError("source material hash/byte mismatch: " + name)
    actual = {p.relative_to(bundle).as_posix() for p in bundle.rglob("*") if p.is_file()}
    if actual != set(files) | {"MANIFEST.json"}:
        raise ValueError("unmanifested source material")
    patches = patch_series((bundle / "baseline/ports/ffmpeg/portfile.cmake").read_bytes().decode())
    if patches != manifest["patches"] or not set(patches["ordered_files"]) <= files.keys():
        raise ValueError("source bundle patch order/comments/material mismatch")
    for name in patches["ordered_files"]:
        overlay_name = name.replace("baseline/ports/", "overlay/", 1)
        # Windows Git checkouts can use CRLF; retain and hash both exact byte
        # forms while requiring the patch content to match the baseline.
        if overlay_name not in files or (bundle / overlay_name).read_bytes().replace(b"\r\n", b"\n") != (bundle / name).read_bytes().replace(b"\r\n", b"\n"):
            raise ValueError("overlay retained patch differs from baseline: " + name)


def stage(source, dest, expected):
    verify(source, expected)
    shutil.copytree(source, dest)
    verify(dest, expected)


if __name__ == "__main__":
    action, *args = sys.argv[1:]
    if action == "create":
        create(*(Path(p).resolve() for p in args))
    elif action == "stage":
        source, dest, sha256, size = args
        expected = {"sha256": sha256, "bytes": int(size)}
        stage(Path(source), Path(dest), expected)
    else:
        raise ValueError("expected create or stage")
