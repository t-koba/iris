"""Generate exact-context adaptations for pinned vcpkg FFmpeg patches."""
import difflib
import hashlib
import json
import re
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parent
PINS = json.loads((ROOT / "pins.json").read_text(encoding="utf-8-sig"))
PIN = PINS["vcpkg"]["ffmpeg_dependency_patch"]
LIB_NAMING_PIN = PINS["vcpkg"]["ffmpeg_lib_naming_patch"]
BIN2C_PIN = PINS["vcpkg"]["ffmpeg_bin2c_patch"]

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


def validate_baseline_patch(path, pin=PIN):
    path = Path(path)
    # Git may check out the patch as CRLF on Windows. Validate the pinned Git
    # blob digest over normalized text; source-bundle.py retains exact Git blobs.
    blob_text = path.read_bytes().replace(b"\r\n", b"\n")
    digest = hashlib.sha256(blob_text).hexdigest()
    if path.name != pin["name"] or digest != pin["sha256"]:
        raise ValueError(f"vcpkg FFmpeg patch {pin['name']} does not match its pinned baseline bytes")
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


def adapt_lib_naming_configure(text):
    if "\r\n" in text and "\n" in text.replace("\r\n", ""):
        raise ValueError("configure has mixed line endings")
    newline = "\r\n" if "\r\n" in text else "\n"
    normalized = text.replace("\r\n", "\n")
    before = ("            -lz)                  echo ${zlib_msvc_lib:-zlib.lib} ;;\n"
              "            -lx264)               echo libx264.lib ;;\n")
    after = ("            -lz)                  echo ${zlib_msvc_lib:-zlib.lib} ;;\n"
             "            -lmp3lame)            echo libmp3lame.lib ;;\n"
             "            -lx264)               echo libx264.lib ;;\n")
    if normalized.count(before) != 1:
        raise ValueError("expected exactly one pinned MSVC zlib/x264 library-name context")
    if "            -lmp3lame)            echo libmp3lame.lib ;;" in normalized:
        raise ValueError("pinned MSVC mp3lame library mapping already exists")
    return normalized.replace(before, after, 1).replace("\n", newline)


def generate_lib_naming(configure_path, baseline_patch_path, output_path):
    validate_baseline_patch(baseline_patch_path, LIB_NAMING_PIN)
    configure_path = Path(configure_path)
    with configure_path.open("r", encoding="utf-8", newline="") as stream:
        before = stream.read()
    after = adapt_lib_naming_configure(before)
    patch = patch_text(before, after)
    Path(output_path).write_bytes(patch.encode("utf-8"))
    return hashlib.sha256(patch.encode("utf-8")).hexdigest()


def adapt_bin2c(text, baseline_patch):
    # The caller verifies the patch hash first. Read its exact deletion anchors
    # rather than duplicating the baseline's assignment values/host-link recipe.
    removed = [line[1:] for line in baseline_patch.splitlines()
               if line.startswith("-") and not line.startswith("---") and line != "-"]
    if (len(removed) != 4 or not removed[0].startswith("BIN2CEXE = ")
            or not removed[1].startswith("BIN2C = ")
            or removed[2] != "$(BIN2CEXE): ffbuild/bin2c_host.o"
            or not removed[3].startswith("\t")):
        raise ValueError("unexpected pinned bin2c patch deletion anchors")
    if "\r" in text:
        raise ValueError("pinned common.mak must use LF line endings")
    # Also reject partial duplicates: an exact block plus a changed assignment
    # or rule must not leave competing definitions behind.
    for pattern in (r"^[ \t]*BIN2CEXE[ \t]*[:?+!]?=", r"^[ \t]*BIN2C[ \t]*[:?+!]?=",
                    r"^[ \t]*\$\(BIN2CEXE\)[ \t]*:"):
        if len(re.findall(pattern, text, re.MULTILINE)) != 1:
            raise ValueError("expected unique pinned bin2c assignment/rule anchors")
    for lines, replacement in ((removed[:2], "BIN2C = bin2c\n"), (removed[2:], "")):
        before = "\n".join(lines) + "\n"
        if lines == removed[2:] and before + "\n" not in text:
            raise ValueError("pinned bin2c host-link rule changed or has extra recipe lines")
        if text.count(before) != 1:
            raise ValueError("expected exactly one pinned bin2c source context")
        text = text.replace(before, replacement, 1)
    return text


def generate_bin2c(source_path, baseline_patch_path, output_path):
    validate_baseline_patch(baseline_patch_path, BIN2C_PIN)
    baseline = Path(baseline_patch_path).read_text(encoding="utf-8")
    before = Path(source_path).read_bytes().decode("utf-8")
    after = adapt_bin2c(before, baseline)
    target = "ffbuild/common.mak"
    patch = f"diff --git a/{target} b/{target}\n" + "".join(difflib.unified_diff(
        before.splitlines(keepends=True), after.splitlines(keepends=True),
        fromfile=f"a/{target}", tofile=f"b/{target}", n=3,
    ))
    Path(output_path).write_bytes(patch.encode("utf-8"))
    return hashlib.sha256(patch.encode("utf-8")).hexdigest()


if __name__ == "__main__":
    if len(sys.argv) != 5:
        raise SystemExit("usage: ffmpeg-dependency-patch.py <adapt|adapt-lib-naming|adapt-bin2c> <source-target> <baseline-patch> <output-patch>")
    if sys.argv[1] == "adapt":
        digest = generate(sys.argv[2], sys.argv[3], sys.argv[4])
        print(f"generated pinned dependency patch sha256={digest}")
    elif sys.argv[1] == "adapt-lib-naming":
        digest = generate_lib_naming(sys.argv[2], sys.argv[3], sys.argv[4])
        print(f"generated pinned library-name patch sha256={digest}")
    elif sys.argv[1] == "adapt-bin2c":
        digest = generate_bin2c(sys.argv[2], sys.argv[3], sys.argv[4])
        print(f"generated pinned prebuilt-bin2c patch sha256={digest}")
    else:
        raise SystemExit("expected adapt, adapt-lib-naming or adapt-bin2c")
