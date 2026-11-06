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
DEPENDENCY_PATCH = PINS["vcpkg"]["ffmpeg_dependency_patch"]
ARCHIVE = f"ffmpeg-{COMMIT}.tar"
INPUTS = (
    "pins.json", "vcpkg.json", "vcpkg-configuration.json",
    "triplets/x64-windows-prototype-dynamic.cmake",
    "derive-ffmpeg-overlay.ps1", "test-overlay-derivation.ps1",
    "ffmpeg-dependency-patch.py", "test-ffmpeg-dependency-patch.py",
    "build-windows-ffmpeg-prototype.ps1", "assert-windows-toolchain-prototype.ps1",
    "source-bundle.py", "test-source-bundle.py", "README.md",
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


def patch_series(port, prefix):
    # Preserve the entire ordered block, including whitespace and comments.
    port = port.lstrip("\ufeff")
    blocks = re.findall(r"(?ms)^vcpkg_from_(?:github|git)\(.*?\bPATCHES\s*\n(.*?)^\)", port)
    if len(blocks) != 1:
        raise ValueError("expected one FFmpeg PATCHES block")
    names = []
    for line in blocks[0].splitlines():
        entry = line.split("#", 1)[0].strip().strip('"')
        if not entry:
            continue
        if not re.fullmatch(r"[A-Za-z0-9_-][A-Za-z0-9_./-]*\.patch", entry) or ".." in entry.split("/"):
            raise ValueError("unsupported patch path")
        names.append(entry)
    if not names or len(set(names)) != len(names):
        raise ValueError("empty or duplicate patch series")
    return {"block": blocks[0], "ordered_files": [prefix.rstrip("/") + "/" + name for name in names]}


def patch_names(series):
    return [PurePosixPath(path).name for path in series["ordered_files"]]


def expected_overlay_names(baseline_names):
    result = []
    for name in baseline_names:
        if name == DEPENDENCY_PATCH["name"]:
            result.append(DEPENDENCY_PATCH["overlay_name"])
        else:
            result.append(name)
    if result.count(DEPENDENCY_PATCH["overlay_name"]) != 1:
        raise ValueError("expected exactly one pinned dependency patch replacement")
    return result


def validate_baseline_patch(path):
    if Path(path).name != DEPENDENCY_PATCH["name"]:
        raise ValueError("unexpected baseline dependency patch filename")
    if hashlib.sha256(Path(path).read_bytes()).hexdigest() != DEPENDENCY_PATCH["sha256"]:
        raise ValueError("vcpkg baseline dependency patch digest mismatch")


def validate_adapted_patch(path):
    # The generated patch may carry CRLF hunk lines on Windows; normalize
    # only for structural checks while the file manifest hashes its exact bytes.
    data = Path(path).read_bytes().replace(b"\r\n", b"\n")
    header = b"diff --git a/configure b/configure\n"
    if not data.startswith(header) or data.count(b"diff --git ") != 1:
        raise ValueError("adapted dependency patch must change only pinned configure")
    if data.count(b"--- a/configure") != 1 or data.count(b"+++ b/configure") != 1:
        raise ValueError("adapted dependency patch has unexpected file paths")
    required = (
        b"require_pkg_config zlib zlib",
        b"-llibmp3lame-static -llibmpghip-static",
        b"openmpt_module_create\n",
        b"require_pkg_config libsnappy snappy",
        b"soxr_create -lsoxr $libm_extralibs",
        b"-lOpenCL -pthread -ldl",
        b"libc_iconv && disabled iconv",
    )
    if any(token not in data for token in required):
        raise ValueError("adapted dependency patch is missing a required vcpkg patch intent")


def validate_patch_manifest(bundle, manifest):
    baseline_port = (bundle / "baseline/ports/ffmpeg/portfile.cmake").read_bytes().decode("utf-8-sig")
    overlay_port = (bundle / "overlay/ffmpeg/portfile.cmake").read_bytes().decode("utf-8-sig")
    baseline = patch_series(baseline_port, "baseline/ports/ffmpeg")
    overlay = patch_series(overlay_port, "overlay/ffmpeg")
    baseline_names = patch_names(baseline)
    overlay_names = patch_names(overlay)
    if overlay_names != expected_overlay_names(baseline_names):
        raise ValueError("overlay patch order differs beyond the one pinned dependency adaptation")
    expected = {
        "baseline": baseline,
        "overlay": overlay,
        "adaptation": {
            "baseline": "baseline/ports/ffmpeg/" + DEPENDENCY_PATCH["name"],
            "overlay": "overlay/ffmpeg/" + DEPENDENCY_PATCH["overlay_name"],
            "baseline_sha256": DEPENDENCY_PATCH["sha256"],
            "kind": "exact-context configure-only adaptation",
        },
    }
    if expected != manifest["patches"]:
        raise ValueError("source bundle patch order/comments/adaptation metadata mismatch")
    if not set(baseline["ordered_files"] + overlay["ordered_files"]) <= manifest["files"].keys():
        raise ValueError("source bundle patch material is missing")
    baseline_path = bundle / expected["adaptation"]["baseline"]
    adapted_path = bundle / expected["adaptation"]["overlay"]
    validate_baseline_patch(baseline_path)
    validate_adapted_patch(adapted_path)
    for baseline_file, overlay_file in zip(baseline["ordered_files"], overlay["ordered_files"]):
        if baseline_file == expected["adaptation"]["baseline"]:
            continue
        if (bundle / overlay_file).read_bytes().replace(b"\r\n", b"\n") != (bundle / baseline_file).read_bytes().replace(b"\r\n", b"\n"):
            raise ValueError("overlay retained patch differs from baseline: " + baseline_file)


def put(dest, name, data):
    path = dest / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def create(source, vcpkg, overlay, dest):
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    # git archive reads committed bytes, not the patched checkout. Disable
    # autocrlf and pin tar permissions so bytes ignore checkout settings/umask.
    subprocess.run(["git", "-C", str(source), "-c", "core.autocrlf=false",
                    "-c", "tar.umask=0022", "archive",
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
    validate_baseline_patch(dest / "baseline/ports/ffmpeg" / DEPENDENCY_PATCH["name"])
    for path in sorted(overlay.rglob("*")):
        if path.is_file():
            put(dest, "overlay/ffmpeg/" + path.relative_to(overlay).as_posix(), path.read_bytes())
    for name in INPUTS:
        put(dest, "build-inputs/" + name, (ROOT / name).read_bytes())
    put(dest, "build-inputs/windows-ffmpeg-prototype.yml",
        (ROOT.parent.parent / ".github/workflows/windows-ffmpeg-prototype.yml").read_bytes())
    baseline_patches = patch_series((dest / "baseline/ports/ffmpeg/portfile.cmake").read_bytes().decode("utf-8-sig"), "baseline/ports/ffmpeg")
    overlay_patches = patch_series((dest / "overlay/ffmpeg/portfile.cmake").read_bytes().decode("utf-8-sig"), "overlay/ffmpeg")
    manifest = {
        "schema": 2, "ffmpeg_commit": COMMIT, "vcpkg_baseline": BASELINE,
        "scope": "FFmpeg upstream tree, retained vcpkg FFmpeg port/patches and prototype recipe only. "
                 "Not complete source for other dependencies, vcpkg tooling or SDKs; no legal conclusion.",
        "patches": {
            "baseline": baseline_patches,
            "overlay": overlay_patches,
            "adaptation": {
                "baseline": "baseline/ports/ffmpeg/" + DEPENDENCY_PATCH["name"],
                "overlay": "overlay/ffmpeg/" + DEPENDENCY_PATCH["overlay_name"],
                "baseline_sha256": DEPENDENCY_PATCH["sha256"],
                "kind": "exact-context configure-only adaptation",
            },
        },
        "files": {p.relative_to(dest).as_posix(): record(p)
                  for p in sorted(dest.rglob("*")) if p.is_file()},
    }
    put(dest, "MANIFEST.json", (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode())
    verify(dest, record(dest / "MANIFEST.json"))


def verify(bundle, expected):
    if record(bundle / "MANIFEST.json") != expected:
        raise ValueError("source bundle manifest hash/byte mismatch")
    manifest = json.loads((bundle / "MANIFEST.json").read_text(encoding="utf-8-sig"))
    if (manifest["schema"], manifest["ffmpeg_commit"], manifest["vcpkg_baseline"]) != (2, COMMIT, BASELINE):
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
    validate_patch_manifest(bundle, manifest)


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
