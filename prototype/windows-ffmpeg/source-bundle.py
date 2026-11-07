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
LIB_NAMING_PATCH = PINS["vcpkg"]["ffmpeg_lib_naming_patch"]
PATCH_ADAPTATIONS = (DEPENDENCY_PATCH, LIB_NAMING_PATCH)
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
    replacements = {item["name"]: item["overlay_name"] for item in PATCH_ADAPTATIONS}
    result = [replacements.get(name, name) for name in baseline_names]
    for item in PATCH_ADAPTATIONS:
        if baseline_names.count(item["name"]) != 1 or result.count(item["overlay_name"]) != 1:
            raise ValueError("expected exactly one pinned patch replacement: " + item["name"])
    return result


def validate_baseline_patch(path, pin):
    if Path(path).name != pin["name"]:
        raise ValueError("unexpected baseline patch filename: " + pin["name"])
    if hashlib.sha256(Path(path).read_bytes()).hexdigest() != pin["sha256"]:
        raise ValueError("vcpkg baseline patch digest mismatch: " + pin["name"])


def validate_adapted_patch(path, pin):
    # The generated patch may carry CRLF hunk lines on Windows; normalize
    # only for structural checks while the file manifest hashes exact bytes.
    data = Path(path).read_bytes().replace(b"\r\n", b"\n")
    header = b"diff --git a/configure b/configure\n"
    if not data.startswith(header) or data.count(b"diff --git ") != 1:
        raise ValueError("adapted patch must change only pinned configure: " + pin["name"])
    if data.count(b"--- a/configure") != 1 or data.count(b"+++ b/configure") != 1:
        raise ValueError("adapted patch has unexpected file paths: " + pin["name"])
    if pin == DEPENDENCY_PATCH:
        required = (
            b"require_pkg_config zlib zlib",
            b"-llibmp3lame-static -llibmpghip-static",
            b"openmpt_module_create\n",
            b"require_pkg_config libsnappy snappy",
            b"soxr_create -lsoxr $libm_extralibs",
            b"-lOpenCL -pthread -ldl",
            b"libc_iconv && disabled iconv",
        )
    elif pin == LIB_NAMING_PATCH:
        required = (
            b"             -lz)                  echo ${zlib_msvc_lib:-zlib.lib} ;;\n",
            b"+            -lmp3lame)            echo libmp3lame.lib ;;\n",
        )
    else:
        raise ValueError("unrecognized pinned patch adaptation")
    if any(token not in data for token in required):
        raise ValueError("adapted patch is missing required baseline patch intent: " + pin["name"])
    if pin == LIB_NAMING_PATCH:
        added = [line for line in data.splitlines() if line.startswith(b"+") and not line.startswith(b"+++")]
        removed = [line for line in data.splitlines() if line.startswith(b"-") and not line.startswith(b"---")]
        expected = [b"+            -lmp3lame)            echo libmp3lame.lib ;;"]
        if added != expected or removed:
            raise ValueError("adapted library-name patch must only add the mp3lame mapping")
        if b"-            -lz)" in data:
            raise ValueError("adapted library-name patch must preserve the dynamic zlib mapping")


def validate_patch_manifest(bundle, manifest):
    baseline_port = (bundle / "baseline/ports/ffmpeg/portfile.cmake").read_bytes().decode("utf-8-sig")
    overlay_port = (bundle / "overlay/ffmpeg/portfile.cmake").read_bytes().decode("utf-8-sig")
    baseline = patch_series(baseline_port, "baseline/ports/ffmpeg")
    overlay = patch_series(overlay_port, "overlay/ffmpeg")
    baseline_names = patch_names(baseline)
    overlay_names = patch_names(overlay)
    if overlay_names != expected_overlay_names(baseline_names):
        raise ValueError("overlay patch order differs beyond the pinned patch adaptations")
    adaptations = [
        {
            "baseline": "baseline/ports/ffmpeg/" + item["name"],
            "overlay": "overlay/ffmpeg/" + item["overlay_name"],
            "baseline_sha256": item["sha256"],
            "kind": "exact-context configure-only adaptation",
        }
        for item in PATCH_ADAPTATIONS
    ]
    expected = {"baseline": baseline, "overlay": overlay, "adaptations": adaptations}
    if expected != manifest["patches"]:
        raise ValueError("source bundle patch order/comments/adaptation metadata mismatch")
    if not set(baseline["ordered_files"] + overlay["ordered_files"]) <= manifest["files"].keys():
        raise ValueError("source bundle patch material is missing")
    adapted_baselines = set()
    for item, adaptation in zip(PATCH_ADAPTATIONS, adaptations):
        validate_baseline_patch(bundle / adaptation["baseline"], item)
        validate_adapted_patch(bundle / adaptation["overlay"], item)
        adapted_baselines.add(adaptation["baseline"])
    for baseline_file, overlay_file in zip(baseline["ordered_files"], overlay["ordered_files"]):
        if baseline_file in adapted_baselines:
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
    for item in PATCH_ADAPTATIONS:
        validate_baseline_patch(dest / "baseline/ports/ffmpeg" / item["name"], item)
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
        "schema": 3, "ffmpeg_commit": COMMIT, "vcpkg_baseline": BASELINE,
        "scope": "FFmpeg upstream tree, retained vcpkg FFmpeg port/patches and prototype recipe only. "
                 "Not complete source for other dependencies, vcpkg tooling or SDKs; no legal conclusion.",
        "patches": {
            "baseline": baseline_patches,
            "overlay": overlay_patches,
            "adaptations": [
                {
                    "baseline": "baseline/ports/ffmpeg/" + item["name"],
                    "overlay": "overlay/ffmpeg/" + item["overlay_name"],
                    "baseline_sha256": item["sha256"],
                    "kind": "exact-context configure-only adaptation",
                }
                for item in PATCH_ADAPTATIONS
            ],
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
    if (manifest["schema"], manifest["ffmpeg_commit"], manifest["vcpkg_baseline"]) != (3, COMMIT, BASELINE):
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
