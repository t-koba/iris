"""Offline exact-context fixture for the pinned FFmpeg dependency adaptation."""
import importlib.util
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

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


# Bounded source-context fixture, not a full upstream tree/native build.
MSVC_UTF8_FIXTURE = b'''check_cflags(){
    log check_cflags "$@"
    test_cflags "$@" && add_cflags "$@"
}

check_cxxflags(){
    log check_cxxflags "$@"
    test_cxxflags "$@" && add_cxxflags "$@"
}

check_allcflags(){
    check_cxxflags "$@"
    check_cflags "$@"
}

elif enabled_any msvc icl; then
    enabled x86_32 && disable aligned_stack
    enabled_all x86_32 debug && add_allcflags -Oy-
    enabled debug && add_ldflags -debug
    enable pragma_deprecated
    if enabled icl; then
        # -Qansi-alias is basically -fstrict-aliasing, but does not work
        # (correctly) on icl 13.x.
        test_cpp_condition "windows.h" "__ICL < 1300 || __ICL >= 1400" &&
            add_allcflags -Qansi-alias
        # Some inline asm is not compilable in debug
        if enabled debug; then
            disable ebx_available
        fi
    fi
    # enable utf-8 source processing
    check_allcflags -utf-8
fi
'''
MSVC_UTF8_BASELINE_PATCH = b"synthetic UTF-8 baseline patch\n"
OPENGL_BASELINE_STANZA = '''if("opengl" IN_LIST FEATURES)
    set(OPTIONS "${OPTIONS} --enable-opengl")
else()
    set(OPTIONS "${OPTIONS} --disable-opengl")
endif()
'''
OPENGL_BASELINE_DISABLE = '    set(OPTIONS "${OPTIONS} --disable-opengl")\n'


class OpenGlPortTests(unittest.TestCase):
    def test_proof_bound_single_line_omission(self):
        self.assertEqual(patcher.OPENGL_STANZA, OPENGL_BASELINE_STANZA)
        self.assertEqual(patcher.OPENGL_DISABLE, OPENGL_BASELINE_DISABLE)
        # Synthetic configure only; never claim this fixture is upstream proof.
        configure = b"no OpenGL options in fixture\n"
        pin = {"configure": hashlib.sha256(configure).hexdigest()}
        for ending in ("\n", "\r\n"):
            port = ("keep before\n" + OPENGL_BASELINE_STANZA + "keep after\n").replace("\n", ending)
            with patch.dict(patcher.SWSCALE_PIN, source_sha256=pin):
                result, evidence = patcher.omit_opengl_disable(port, configure)
                self.assertEqual(result, port.replace(patcher.OPENGL_DISABLE.replace("\n", ending), ""))
                self.assertEqual(evidence["source_sha256"], pin["configure"])
                self.assertEqual(evidence["removed_option"], "--disable-opengl")
                self.assertEqual(evidence["overlay_disable_occurrences"], 0)
                for bad in (result, port + port, port.replace('else()', 'else( )'),
                            port.replace('--enable-opengl', '--enable-other'),
                            port + '--disable-opengl', port + 'if("opengl" IN_LIST FEATURES)',
                            port.replace('if("opengl"', '#if("opengl"'),
                            port.replace('else()' + ending, 'else()\r\n' if ending == "\n" else 'else()\n')):
                    with self.subTest(port=bad), self.assertRaises(ValueError):
                        patcher.omit_opengl_disable(bad, configure)
                with self.assertRaises(ValueError):
                    patcher.omit_opengl_disable(port, configure + b"drift")
            # Even a matching digest cannot authorize omission if either option
            # appears in configure; this guard must not rely only on the hash.
            for option in (b"--enable-opengl", b"--disable-opengl"):
                supported = configure + option
                with patch.dict(patcher.SWSCALE_PIN, source_sha256={
                        "configure": hashlib.sha256(supported).hexdigest()}):
                    with self.assertRaises(ValueError):
                        patcher.omit_opengl_disable(port, supported)


class MsvcUtf8Tests(unittest.TestCase):
    def test_unique_context_and_helper_semantics(self):
        for source in (MSVC_UTF8_FIXTURE, MSVC_UTF8_FIXTURE.replace(b"\n", b"\r\n")):
            evidence = patcher.validate_msvc_utf8(source)
            self.assertEqual(evidence["source_sha256"], hashlib.sha256(source).hexdigest())
            self.assertEqual(evidence["baseline_sha256"],
                             "76e199ae27fc8f8be518ffa45d07c152abc3a0e05e42130ae5d6b54a26bc5683")
        for context in patcher.MSVC_UTF8_CONTEXTS.values():
            for source in (MSVC_UTF8_FIXTURE.replace(context.encode(), b""),
                           MSVC_UTF8_FIXTURE + context.encode(),
                           MSVC_UTF8_FIXTURE + context.replace("$@", "ignored").encode()):
                with self.assertRaises(ValueError):
                    patcher.validate_msvc_utf8(source)
        for before, after in ((b"check_allcflags -utf-8", b"check_cflags -utf-8"),
                              (b'    check_cxxflags "$@"', b'    : "$@"'),
                              (b'    check_cflags "$@"', b'    : "$@"'),
                              (b'&& add_cxxflags', b'&& log'),
                              (b'&& add_cflags', b'&& log')):
            with self.assertRaises(ValueError):
                patcher.validate_msvc_utf8(MSVC_UTF8_FIXTURE.replace(before, after))

    def test_hash_guard_before_omission_evidence(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, baseline, output = root / "configure", root / patcher.MSVC_UTF8_PIN["name"], root / "evidence.json"
            source.write_bytes(MSVC_UTF8_FIXTURE)
            baseline.write_bytes(MSVC_UTF8_BASELINE_PATCH)
            with self.assertRaisesRegex(ValueError, "pinned baseline bytes"):
                patcher.omit_msvc_utf8(source, baseline, output)
            self.assertFalse(output.exists())
            with patch.dict(patcher.MSVC_UTF8_PIN, sha256=hashlib.sha256(MSVC_UTF8_BASELINE_PATCH).hexdigest()):
                patcher.omit_msvc_utf8(source, baseline, output)
                self.assertEqual(json.loads(output.read_text()), patcher.validate_msvc_utf8(source.read_bytes()))
            self.assertEqual(source.read_bytes(), MSVC_UTF8_FIXTURE)


# Synthetic, bounded context fixture, not fetched FFmpeg or native build proof.
# Tests explicitly override pins only for positive synthetic cases.
SWSCALE_BASELINE_PATCH = b"synthetic unstable swscale baseline patch\n"
SWSCALE_FIXTURE = {
    "configure": MSVC_UTF8_FIXTURE + b'swscale_vulkan_deps="vulkan unstable"\n',
    "libswscale/format.c": b"""void ff_sws_chroma_pos(int *pos)
{
}
static inline SwsLinearOp
linear_mat3(const SwsPixelType type,
            const AVRational m00)
{
}
static void decode_colors(void *colors)
{
}
""",
    "libswscale/graph.c": b"#if CONFIG_SWSCALE_VULKAN\ncomponent();\n#endif\n",
    "libswscale/Makefile": b"OBJS = format.o \\\n       graph.o\n",
    "libswscale/vulkan/Makefile": b"OBJS-$(CONFIG_SWSCALE_VULKAN) += vulkan/ops.o \\\n       vulkan/input.o\n",
}
SWSCALE_HASHES = {name: hashlib.sha256(SWSCALE_FIXTURE[name]).hexdigest()
                  for name in patcher.SWSCALE_PIN["source_sha256"]}


class SwscaleTests(unittest.TestCase):
    def test_pristine_hash_drift_is_not_supersession(self):
        self.assertEqual(patcher.SWSCALE_PIN["sha256"],
                         "994fda8511b76943b9843764d52926ee8544c58fb337940ac8e617b50cdff879")
        with self.assertRaisesRegex(ValueError, "source digest mismatch"):
            patcher.validate_unstable_swscale(SWSCALE_FIXTURE)
        with patch.dict(patcher.SWSCALE_PIN, source_sha256=SWSCALE_HASHES):
            evidence = patcher.validate_unstable_swscale(SWSCALE_FIXTURE)
            self.assertEqual(evidence["ffmpeg_commit"], "db8537d8cc7962bd3d07254d6ceff21d888817d4")
            self.assertEqual(evidence["vcpkg_baseline"], "b8dfe957ec8b5c0ef5c4b31eea594b8486af744f")
            self.assertEqual(evidence["source_sha256"], {
                name: hashlib.sha256(data).hexdigest() for name, data in SWSCALE_FIXTURE.items()})
            self.assertEqual(evidence["contexts_sha256"], {
                name: hashlib.sha256(context.encode()).hexdigest()
                for name, context in patcher.swscale_contexts(SWSCALE_FIXTURE).items()})
            self.assertLess(len(json.dumps(evidence)), 5000)
            for name in SWSCALE_HASHES:
                for data in (b"", SWSCALE_FIXTURE[name] * 2, SWSCALE_FIXTURE[name] + b"drift"):
                    with self.subTest(name=name, data=data), self.assertRaisesRegex(ValueError, "source digest mismatch"):
                        patcher.validate_unstable_swscale({**SWSCALE_FIXTURE, name: data})

    def test_missing_duplicate_and_altered_contexts(self):
        # Check semantics separately so the hash guard cannot mask a weak context check.
        for name in SWSCALE_FIXTURE:
            with self.subTest(name=name), self.assertRaises(ValueError):
                patcher.swscale_contexts({**SWSCALE_FIXTURE, name: b""})
        for name in ("configure", "libswscale/format.c", "libswscale/Makefile",
                     "libswscale/vulkan/Makefile"):
            with self.subTest(duplicate=name), self.assertRaises(ValueError):
                patcher.swscale_contexts({**SWSCALE_FIXTURE, name: SWSCALE_FIXTURE[name] * 2})
        mutations = (
            ("configure", b"vulkan unstable", b"vulkan"),
            ("configure", b'swscale_vulkan_deps=', b' swscale_vulkan_deps='),
            ("libswscale/format.c", b"static void decode_colors", b"void ff_sws_decode_colors"),
            ("libswscale/format.c", b"static void decode_colors", b"void decode_colors"),
            ("libswscale/format.c", b"void ff_sws_chroma_pos", b"#if CONFIG_UNSTABLE\nvoid ff_sws_chroma_pos"),
            ("libswscale/graph.c", b"CONFIG_SWSCALE_VULKAN", b"CONFIG_VULKAN"),
            ("libswscale/Makefile", b"OBJS =", b"OBJS-$(CONFIG_UNSTABLE) ="),
            ("libswscale/vulkan/Makefile", b"OBJS-$(CONFIG_SWSCALE_VULKAN)", b"OBJS"),
        )
        for name, before, after in mutations:
            with self.subTest(name=name, after=after), self.assertRaises(ValueError):
                patcher.swscale_contexts({**SWSCALE_FIXTURE, name: SWSCALE_FIXTURE[name].replace(before, after)})
        with self.assertRaises(ValueError):
            patcher.swscale_contexts({**SWSCALE_FIXTURE, "configure": SWSCALE_FIXTURE["configure"] +
                                     b'swscale_vulkan_deps="vulkan"\n'})

    def test_omission_evidence_and_sequential_stage_guards(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name, data in SWSCALE_FIXTURE.items():
                target = root / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            baseline, output = root / patcher.SWSCALE_PIN["name"], root / "evidence.json"
            baseline.write_bytes(SWSCALE_BASELINE_PATCH)
            with patch.object(patcher.subprocess, "check_output") as git:
                with self.assertRaisesRegex(ValueError, "pinned baseline bytes"):
                    patcher.omit_unstable_swscale(root, baseline, output)
                git.assert_not_called()
            self.assertFalse(output.exists())
            def git_blob(args):
                self.assertEqual(args[:4], ["git", "-C", str(root), "show"])
                self.assertTrue(args[-1].startswith(patcher.PINS["ffmpeg"]["commit"] + ":"))
                return SWSCALE_FIXTURE[args[-1].split(":", 1)[1]]
            with patch.dict(patcher.SWSCALE_PIN, source_sha256=SWSCALE_HASHES,
                            sha256=hashlib.sha256(SWSCALE_BASELINE_PATCH).hexdigest()), \
                    patch.object(patcher.subprocess, "check_output", side_effect=git_blob):
                # Authorized prior configure patches may alter unrelated lines.
                (root / "configure").write_bytes(b"# earlier dependency adaptation\n" + SWSCALE_FIXTURE["configure"])
                patcher.omit_unstable_swscale(root, baseline, output)
                evidence = json.loads(output.read_text())
                self.assertEqual(evidence["contexts_sha256"], patcher.validate_unstable_swscale(SWSCALE_FIXTURE)["contexts_sha256"])
                self.assertNotEqual(evidence["sequential_source_sha256"]["configure"], evidence["source_sha256"]["configure"])
                output.unlink()
                for name, bad in (("configure", SWSCALE_FIXTURE["configure"].replace(b"vulkan unstable", b"vulkan")),
                                  ("libswscale/format.c", SWSCALE_FIXTURE["libswscale/format.c"] + b"// drift\n")):
                    (root / name).write_bytes(bad)
                    with self.assertRaises(ValueError):
                        patcher.omit_unstable_swscale(root, baseline, output)
                    self.assertFalse(output.exists())
                    (root / name).write_bytes(SWSCALE_FIXTURE[name])
                baseline.write_bytes(SWSCALE_BASELINE_PATCH + b"drift")
                with self.assertRaises(ValueError):
                    patcher.omit_unstable_swscale(root, baseline, output)
                self.assertFalse(output.exists())


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



# Synthetic baseline shape; its hash is substituted only for the positive
# generator fixture. This is not upstream provenance. Production uses BIN2C_PIN.
BIN2C_ASSIGNMENTS = "BIN2CEXE = ffbuild/bin2c_host$(HOSTEXESUF)\nBIN2C = $(BIN2CEXE)\n"
BIN2C_RULE = "$(BIN2CEXE): ffbuild/bin2c_host.o\n\t$(HOSTLD) $(HOSTLDFLAGS) $(HOSTLD_O) $^ $(HOSTEXTRALIBS)\n"
BIN2C_BASELINE_PATCH = ("--- a/ffbuild/common.mak\n+++ b/ffbuild/common.mak\n"
                        "@@ -1,4 +1,3 @@\n"
                        + "".join("-" + line for line in BIN2C_ASSIGNMENTS.splitlines(keepends=True))
                        + "+BIN2C = bin2c\n \n ifndef V\n@@ -20,3 +19,0 @@\n"
                        + "".join("-" + line for line in BIN2C_RULE.splitlines(keepends=True)) + "-\n")
BIN2C_COMMENT = "# INSTALL_FILES is used for installation\n"
BIN2C_FIXTURE = ("# preceding patch already applied\n" + BIN2C_ASSIGNMENTS + "\n"
                 + BIN2C_COMMENT + "\nifndef V\nQ = @\nendif\n\n"
                 + BIN2C_RULE + "\nother: other.o\n\t$(HOSTLD) $^\n")


class Bin2cPatchTests(unittest.TestCase):
    def test_exact_transformation_generates_common_mak_patch_at_sequential_stage(self):
        expected = BIN2C_FIXTURE.replace(BIN2C_ASSIGNMENTS, "BIN2C = bin2c\n").replace(BIN2C_RULE, "")
        self.assertEqual(patcher.adapt_bin2c(BIN2C_FIXTURE, BIN2C_BASELINE_PATCH), expected)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(["git", "-C", str(root), "config", "core.autocrlf", "false"], check=True)
            source = root / "ffbuild/common.mak"
            source.parent.mkdir()
            source.write_bytes(BIN2C_FIXTURE.encode())
            baseline = root / patcher.BIN2C_PIN["name"]
            baseline.write_bytes(BIN2C_BASELINE_PATCH.encode())
            output = root / "adapt.patch"
            with patch.dict(patcher.BIN2C_PIN, sha256=hashlib.sha256(baseline.read_bytes()).hexdigest()):
                digest = patcher.generate_bin2c(source, baseline, output)
            self.assertEqual(digest, hashlib.sha256(output.read_bytes()).hexdigest())
            diff = output.read_text()
            self.assertTrue(diff.startswith("diff --git a/ffbuild/common.mak b/ffbuild/common.mak\n"))
            self.assertIn("+++ b/ffbuild/common.mak\n", diff)
            self.assertNotIn("a/configure", diff)
            self.assertIn(" " + BIN2C_COMMENT, diff)
            subprocess.run(["git", "-C", str(root), "apply", "--check", str(output)], check=True)
            subprocess.run(["git", "-C", str(root), "apply", str(output)], check=True)
            self.assertEqual(source.read_text(), expected)

    def test_missing_duplicate_or_changed_anchors_fail_closed(self):
        for anchor in (BIN2C_ASSIGNMENTS, BIN2C_RULE, *BIN2C_ASSIGNMENTS.splitlines(keepends=True),
                       *BIN2C_RULE.splitlines(keepends=True)):
            for broken in (BIN2C_FIXTURE.replace(anchor, ""), BIN2C_FIXTURE.replace(anchor, anchor + anchor),
                           BIN2C_FIXTURE.replace(anchor, "changed" + anchor)):
                with self.subTest(anchor=anchor, broken=broken):
                    with self.assertRaises(ValueError):
                        patcher.adapt_bin2c(broken, BIN2C_BASELINE_PATCH)
        for broken in (BIN2C_FIXTURE + "BIN2C = changed\n",
                       BIN2C_FIXTURE.replace(BIN2C_RULE, BIN2C_RULE + "\tunexpected recipe\n"),
                       BIN2C_FIXTURE.replace("\n", "\r\n")):
            with self.assertRaises(ValueError):
                patcher.adapt_bin2c(broken, BIN2C_BASELINE_PATCH)

    def test_wrong_baseline_hash_prevents_reading_source_or_writing_patch(self):
        self.assertEqual(patcher.BIN2C_PIN["sha256"],
                         "5510a5d6cc0e6cac881157b8432dcd14672e6618c8b9e8719414e1d3c1804e21")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            baseline = root / "0045-use-prebuilt-bin2c.patch"
            baseline.write_text(BIN2C_BASELINE_PATCH)
            output = root / "adapt.patch"
            with self.assertRaisesRegex(ValueError, "pinned baseline bytes"):
                patcher.generate_bin2c(root / "missing-common.mak", baseline, output)
            self.assertFalse(output.exists())

    def test_build_routes_third_adaptation_to_common_mak_in_patch_order(self):
        build = (ROOT / "build-windows-ffmpeg-prototype.ps1").read_text()
        self.assertIn('source = "ffbuild/common.mak"\n        command = "adapt-bin2c"', build)
        self.assertIn('$adaptation.command (Join-Path $workDir $adaptation.source)', build)
        self.assertIn('Set-PrototypeFfmpegPatch $overlayRaw $adaptation.baseline $adaptation.overlay', build)
        self.assertLess(build.index('foreach ($name in $derived.Patches)'),
                        build.index('& python (Join-Path $scriptDir "ffmpeg-dependency-patch.py") $adaptation.command'))


if __name__ == "__main__":
    unittest.main()
