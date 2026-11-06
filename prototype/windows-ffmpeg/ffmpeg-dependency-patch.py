"""Generate a pinned-commit vcpkg 0004 patch; fail closed on baseline or source drift."""
import difflib
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parent
PINS = json.loads((ROOT / "pins.json").read_text(encoding="utf-8-sig"))
PIN = PINS["vcpkg"]["ffmpeg_dependency_patch"]

# Exact pinned-commit configure contexts. These carry the vcpkg 9.0.2 patch's
# dependency/link changes while accepting only the known newer source shape.
REPLACEMENTS = (
    (
        '''enabled  zlib && { check_pkg_config zlib zlib "zlib.h" zlibVersion ||
                   { zlib_msvc_lib=z.lib;    check_lib zlib zlib.h zlibVersion -lz; } ||
                   { zlib_msvc_lib=zlib.lib; check_lib zlib zlib.h zlibVersion -lz; }; }
enabled bzlib && check_lib bzlib bzlib.h BZ2_bzlibVersion    -lbz2
''',
        '''enabled  zlib && require_pkg_config zlib zlib   zlib.h  zlibVersion
enabled bzlib && require_pkg_config bzlib bzip2 bzlib.h BZ2_bzlibVersion
''',
        "zlib/bzip2",
    ),
    (
        '''enabled libmp3lame        && { check_pkg_config libmp3lame "lame >= 3.98.3" lame/lame.h lame_set_VBR_quality ||
                               require "libmp3lame >= 3.98.3" lame/lame.h lame_set_VBR_quality -lmp3lame $libm_extralibs; }
''',
        '''enabled libmp3lame        && { check_lib libmp3lame lame/lame.h lame_set_VBR_quality -lmp3lame $libm_extralibs ||
                                              require libmp3lame lame/lame.h lame_set_VBR_quality -llibmp3lame-static -llibmpghip-static $libm_extralibs; }
''',
        "libmp3lame",
    ),
    (
        '''enabled libopenmpt        && require_pkg_config libopenmpt "libopenmpt >= 0.2.6557" libopenmpt/libopenmpt.h openmpt_module_create -lstdc++ && append libopenmpt_extralibs "-lstdc++"
''',
        '''enabled libopenmpt        && require_pkg_config libopenmpt "libopenmpt >= 0.2.6557" libopenmpt/libopenmpt.h openmpt_module_create
''',
        "libopenmpt",
    ),
    (
        '''enabled libsnappy         && require libsnappy snappy-c.h snappy_compress -lsnappy -lstdc++
enabled libsoxr           && require libsoxr soxr.h soxr_create -lsoxr
''',
        '''enabled libsnappy         && require_pkg_config libsnappy snappy snappy-c.h snappy_compress
enabled libsoxr           && require libsoxr soxr.h soxr_create -lsoxr $libm_extralibs
''',
        "snappy/soxr",
    ),
    (
        '''enabled opencl            && { check_pkg_config opencl OpenCL CL/cl.h clEnqueueNDRangeKernel ||
                               check_lib opencl OpenCL/cl.h clEnqueueNDRangeKernel "-framework OpenCL" ||
                               check_lib opencl CL/cl.h clEnqueueNDRangeKernel -lOpenCL ||
                               die "ERROR: opencl not found"; } &&
''',
        '''enabled opencl            && { check_pkg_config opencl OpenCL CL/cl.h clEnqueueNDRangeKernel ||
                               check_lib opencl OpenCL/cl.h clEnqueueNDRangeKernel "-framework OpenCL" ||
                               check_lib opencl CL/cl.h clEnqueueNDRangeKernel -lOpenCL ||
                               check_lib opencl CL/cl.h clEnqueueNDRangeKernel -lOpenCL -lAdvapi32 -lOle32 -lCfgmgr32||
                               check_lib opencl CL/cl.h clEnqueueNDRangeKernel -lOpenCL -pthread -ldl ||
                               die "ERROR: opencl not found"; } &&
''',
        "OpenCL link fallbacks",
    ),
    (
        '''if enabled libc_iconv; then
    check_func_headers iconv.h iconv
elif enabled iconv; then
    check_func_headers iconv.h iconv || check_lib iconv iconv.h iconv -liconv
fi
''',
        '''if enabled libc_iconv && disabled iconv; then
    check_func_headers iconv.h iconv
elif enabled iconv; then
    check_func_headers iconv.h iconv || check_lib iconv iconv.h iconv -liconv || check_lib iconv iconv.h iconv -liconv -lcharset
fi
''',
        "iconv",
    ),
)


def adapt_configure(text):
    if "\r\n" in text and "\n" in text.replace("\r\n", ""):
        raise ValueError("configure has mixed line endings")
    newline = "\r\n" if "\r\n" in text else "\n"
    normalized = text.replace("\r\n", "\n")
    # This newer upstream probe is intentionally retained; only the subsequent
    # dependency checks are adapted to the exact vcpkg patch intent.
    if "enabled zlib && msvc_style_linker && probe_msvc_zlib_libname\n" not in normalized:
        raise ValueError("pinned MSVC zlib probe is missing or changed")
    for before, after, label in REPLACEMENTS:
        count = normalized.count(before)
        if count != 1:
            raise ValueError(f"expected exactly one pinned {label} context; found {count}")
        normalized = normalized.replace(before, after, 1)
    return normalized.replace("\n", newline)


def patch_text(before, after):
    if before == after:
        raise ValueError("pinned configure adaptation made no changes")
    diff = "".join(difflib.unified_diff(
        before.splitlines(keepends=True), after.splitlines(keepends=True),
        fromfile="a/configure", tofile="b/configure", n=3,
    ))
    if not diff.startswith("--- a/configure") or "+++ b/configure" not in diff:
        raise ValueError("failed to generate a configure-only unified patch")
    return "diff --git a/configure b/configure\n" + diff


def validate_baseline_patch(path):
    path = Path(path)
    # Git may check out the patch as CRLF on Windows. Validate the pinned Git
    # blob digest over normalized text; source-bundle.py separately retains and
    # hashes the exact Git blob bytes from `git show`.
    blob_text = path.read_bytes().replace(b"\r\n", b"\n")
    digest = hashlib.sha256(blob_text).hexdigest()
    if path.name != PIN["name"] or digest != PIN["sha256"]:
        raise ValueError("vcpkg FFmpeg dependency patch does not match its pinned baseline bytes")
    return digest


def generate(configure_path, baseline_patch_path, output_path):
    validate_baseline_patch(baseline_patch_path)
    configure_path = Path(configure_path)
    with configure_path.open("r", encoding="utf-8", newline="") as stream:
        before = stream.read()
    after = adapt_configure(before)
    patch = patch_text(before, after)
    Path(output_path).write_bytes(patch.encode("utf-8"))
    return hashlib.sha256(patch.encode("utf-8")).hexdigest()


if __name__ == "__main__":
    if len(sys.argv) != 5 or sys.argv[1] != "adapt":
        raise SystemExit("usage: ffmpeg-dependency-patch.py adapt <configure> <baseline-0004> <output-patch>")
    digest = generate(sys.argv[2], sys.argv[3], sys.argv[4])
    print(f"generated pinned dependency patch sha256={digest}")
