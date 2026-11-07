"""Offline exact-context fixture for the pinned FFmpeg dependency adaptation."""
import importlib.util
import hashlib
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("ffmpeg_dependency_patch", ROOT / "ffmpeg-dependency-patch.py")
patcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(patcher)
FIXTURE = '''enabled zlib && msvc_style_linker && probe_msvc_zlib_libname
enabled  zlib && { check_pkg_config zlib zlib "zlib.h" zlibVersion ||
                   { zlib_msvc_lib=z.lib;    check_lib zlib zlib.h zlibVersion -lz; } ||
                   { zlib_msvc_lib=zlib.lib; check_lib zlib zlib.h zlibVersion -lz; }; }
enabled bzlib && check_lib bzlib bzlib.h BZ2_bzlibVersion    -lbz2
enabled  lzma && check_lib lzma   lzma.h lzma_version_number -llzma

enabled libmp3lame        && { check_pkg_config libmp3lame "lame >= 3.98.3" lame/lame.h lame_set_VBR_quality ||
                               require "libmp3lame >= 3.98.3" lame/lame.h lame_set_VBR_quality -lmp3lame $libm_extralibs; }
enabled libmpeghdec       && require_pkg_config libmpeghdec "mpeghdec >= 3.0.0" mpeghdec/mpeghdecoder.h mpeghdecoder_init

enabled libopenmpt        && require_pkg_config libopenmpt "libopenmpt >= 0.2.6557" libopenmpt/libopenmpt.h openmpt_module_create -lstdc++ && append libopenmpt_extralibs "-lstdc++"
enabled libopenvino       && require_pkg_config libopenvino openvino openvino/c/openvino.h ov_core_create

enabled libsnappy         && require libsnappy snappy-c.h snappy_compress -lsnappy -lstdc++
enabled libsoxr           && require libsoxr soxr.h soxr_create -lsoxr
enabled libssh            && require_pkg_config libssh "libssh >= 0.6.0" libssh/sftp.h sftp_init

enabled opencl            && { check_pkg_config opencl OpenCL CL/cl.h clEnqueueNDRangeKernel ||
                               check_lib opencl OpenCL/cl.h clEnqueueNDRangeKernel "-framework OpenCL" ||
                               check_lib opencl CL/cl.h clEnqueueNDRangeKernel -lOpenCL ||
                               die "ERROR: opencl not found"; } &&
                              { test_cpp_condition "OpenCL/cl.h" "defined(CL_VERSION_1_2)" ||
                               die "ERROR: opencl must be version 1.2"; }

if enabled libc_iconv; then
    check_func_headers iconv.h iconv
elif enabled iconv; then
    check_func_headers iconv.h iconv || check_lib iconv iconv.h iconv -liconv
fi

enabled debug && add_allcflags -g"$debuglevel" && add_asflags -g"$debuglevel"
'''


LIB_NAMING_FIXTURE = '''            -mfp16-format=*)      ;;
            -lz)                  echo ${zlib_msvc_lib:-zlib.lib} ;;
            -lx264)               echo libx264.lib ;;
            -lstdc++)             ;;
'''
LIB_NAMING_BASELINE_PATCH = b'''diff --git a/configure b/configure
index d6c4388..75b96c3 100644
--- a/configure
+++ b/configure
@@ -4781,6 +4781,6 @@ msvc_common_flags(){
             -mfp16-format=*)      ;;
-            -lz)                  echo zlib.lib ;;
             -lx264)               echo libx264.lib ;;
+            -lmp3lame)            echo libmp3lame.lib ;;
             -lstdc++)             ;;
             -l*)                  echo ${flag#-l}.lib ;;
             -LARGEADDRESSAWARE)   echo $flag ;;
'''


class DependencyPatchTests(unittest.TestCase):
    def test_pinned_source_checkout_keeps_patch_line_endings_compatible(self):
        build = (ROOT / "build-windows-ffmpeg-prototype.ps1").read_text(encoding="utf-8")
        configure = build.index("git -C $workDir config core.autocrlf false")
        checkout = build.index("git checkout --detach $ffmpegCommit --quiet")
        self.assertLess(configure, checkout)
        self.assertIn("$autoCrlf -ne \"false\"", build[configure:checkout])

    def test_exact_contexts_generate_applicable_patch_for_lf_and_crlf(self):
        for original in (FIXTURE, FIXTURE.replace("\n", "\r\n")):
            adapted = patcher.adapt_configure(original)
            self.assertIn("enabled zlib && msvc_style_linker && probe_msvc_zlib_libname", adapted)
            self.assertIn("enabled  zlib && require_pkg_config", adapted)
            self.assertIn("-llibmp3lame-static -llibmpghip-static", adapted)
            self.assertIn("enabled libsnappy         && require_pkg_config", adapted)
            self.assertIn("-lOpenCL -pthread -ldl", adapted)
            self.assertIn("if enabled libc_iconv && disabled iconv; then", adapted)
            self.assertNotIn("append libopenmpt_extralibs", adapted)
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                subprocess.run(["git", "init", "-q", str(root)], check=True)
                # Test the exact fixture bytes, independent of a Windows user's
                # global core.autocrlf setting.
                subprocess.run(["git", "-C", str(root), "config", "core.autocrlf", "false"], check=True)
                path = root / "configure"
                path.write_bytes(original.encode("utf-8"))
                patch = root / "adapt.patch"
                patch.write_text(patcher.patch_text(original, adapted), encoding="utf-8", newline="")
                subprocess.run(["git", "-C", str(root), "apply", "--check", str(patch)], check=True)
                subprocess.run(["git", "-C", str(root), "apply", str(patch)], check=True)
                self.assertEqual(path.read_bytes().decode("utf-8"), adapted)

    def test_source_drift_fails_closed(self):
        drifted = FIXTURE.replace("probe_msvc_zlib_libname", "probe_other_zlib_libname")
        with self.assertRaisesRegex(ValueError, "MSVC zlib probe"):
            patcher.adapt_configure(drifted)
        drifted = FIXTURE.replace("enabled libmp3lame", "enabled libmp3lame_changed")
        with self.assertRaisesRegex(ValueError, "libmp3lame"):
            patcher.adapt_configure(drifted)

    def test_lib_naming_preserves_dynamic_zlib_and_adds_mp3lame(self):
        for original in (LIB_NAMING_FIXTURE, LIB_NAMING_FIXTURE.replace("\n", "\r\n")):
            adapted = patcher.adapt_lib_naming_configure(original)
            self.assertIn("-lz)                  echo ${zlib_msvc_lib:-zlib.lib} ;;", adapted)
            self.assertIn("-lmp3lame)            echo libmp3lame.lib ;;", adapted)
            self.assertEqual(adapted.count("-lmp3lame)"), 1)
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                subprocess.run(["git", "init", "-q", str(root)], check=True)
                subprocess.run(["git", "-C", str(root), "config", "core.autocrlf", "false"], check=True)
                path = root / "configure"
                path.write_bytes(original.encode("utf-8"))
                patch = root / "adapt.patch"
                patch.write_text(patcher.patch_text(original, adapted), encoding="utf-8", newline="")
                subprocess.run(["git", "-C", str(root), "apply", "--check", str(patch)], check=True)
                subprocess.run(["git", "-C", str(root), "apply", str(patch)], check=True)
                self.assertEqual(path.read_bytes().decode("utf-8"), adapted)

    def test_lib_naming_rejects_source_drift_and_duplicate_mapping(self):
        with self.assertRaisesRegex(ValueError, "zlib/x264 library-name context"):
            patcher.adapt_lib_naming_configure(LIB_NAMING_FIXTURE.replace("zlib_msvc_lib", "other_zlib_lib"))
        with self.assertRaisesRegex(ValueError, "already exists"):
            patcher.adapt_lib_naming_configure(
                LIB_NAMING_FIXTURE.replace("            -lstdc++)",
                                           "            -lmp3lame)            echo libmp3lame.lib ;;\n            -lstdc++)"))

    def test_lib_naming_baseline_patch_hash_is_pinned(self):
        self.assertEqual(hashlib.sha256(LIB_NAMING_BASELINE_PATCH).hexdigest(),
                         patcher.LIB_NAMING_PIN["sha256"])
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / patcher.LIB_NAMING_PIN["name"]
            path.write_bytes(LIB_NAMING_BASELINE_PATCH)
            self.assertEqual(patcher.validate_baseline_patch(path, patcher.LIB_NAMING_PIN),
                             patcher.LIB_NAMING_PIN["sha256"])

    def test_baseline_patch_hash_is_pinned(self):
        self.assertEqual(patcher.PIN["name"], "0004-dependencies.patch")
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / patcher.PIN["name"]
            path.write_text("not the pinned patch")
            with self.assertRaisesRegex(ValueError, "pinned baseline bytes"):
                patcher.validate_baseline_patch(path)


if __name__ == "__main__":
    unittest.main()
