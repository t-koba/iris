"""Bounded offline fixtures, not upstream provenance or a Windows build."""
import hashlib
import io
import importlib.util
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("bundle", ROOT / "source-bundle.py")
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)
dep_spec = importlib.util.spec_from_file_location("dependency_patch_test", ROOT / "test-ffmpeg-dependency-patch.py")
dependency_test = importlib.util.module_from_spec(dep_spec)
dep_spec.loader.exec_module(dependency_test)
LIB_NAMING_BASELINE_PATCH = dependency_test.LIB_NAMING_BASELINE_PATCH
PORT = b'''vcpkg_from_github(
    OUT_SOURCE_PATH SOURCE_PATH
    REPO ffmpeg/ffmpeg
    REF "nVERSION"
    SHA512 HASH
    HEAD_REF master
    PATCHES
        z-first.patch # retained inline comment
        "0004-dependencies.patch" # pinned dependency patch
        0005-fix-nasm.patch
        0007-fix-lib-naming.patch
        0045-use-prebuilt-bin2c.patch
        0047-fix-msvc-utf8.patch # superseded upstream
        a-second.patch
)
'''
BASELINE_PATCH = b'''diff --git a/configure b/configure
index 1759694..fcbae49 100644
--- a/configure
+++ b/configure
@@ -7184,7 +7184,7 @@ fi

-enabled  zlib && { check_pkg_config zlib zlib "zlib.h" zlibVersion ||
-                   check_lib zlib   zlib.h      zlibVersion    -lz; }
-enabled bzlib && check_lib bzlib bzlib.h BZ2_bzlibVersion    -lbz2
+
+enabled  zlib && require_pkg_config zlib zlib   zlib.h  zlibVersion
+enabled bzlib && require_pkg_config bzlib bzip2 bzlib.h BZ2_bzlibVersion
 enabled  lzma && check_lib lzma   lzma.h lzma_version_number -llzma

 enabled zlib && test_exec $zlib_extralibs <<EOF && enable zlib_gzip
@@ -7322,7 +7322,8 @@ if enabled libmfx; then
 fi

 enabled libmodplug        && require_pkg_config libmodplug libmodplug libmodplug/modplug.h ModPlug_Load
-enabled libmp3lame        && require "libmp3lame >= 3.98.3" lame/lame.h lame_set_VBR_quality -lmp3lame $libm_extralibs
+enabled libmp3lame        && { check_lib libmp3lame lame/lame.h lame_set_VBR_quality -lmp3lame $libm_extralibs ||
+                                              require libmp3lame lame/lame.h lame_set_VBR_quality -llibmp3lame-static -llibmpghip-static $libm_extralibs; }
 enabled libmpeghdec       && require_pkg_config libmpeghdec "mpeghdec >= 3.0.0" mpeghdec/mpeghdecoder.h mpeghdecoder_init
 enabled libmysofa         && { check_pkg_config libmysofa libmysofa mysofa.h mysofa_neighborhood_init_withstepdefine ||
                                require libmysofa mysofa.h mysofa_neighborhood_init_withstepdefine -lmysofa $zlib_extralibs; }
@@ -7343,7 +7344,7 @@ enabled libopencolorio    && require_pkg_config_cxx libopencolorio "OpenColorIO"
 enabled libopenh264       && require_pkg_config libopenh264 "openh264 >= 1.3.0" wels/codec_api.h WelsGetCodecVersion
 enabled libopenjpeg       && { check_pkg_config libopenjpeg "libopenjp2 >= 2.1.0" openjpeg.h opj_version ||
                                { require_pkg_config libopenjpeg "libopenjp2 >= 2.1.0" openjpeg.h opj_version -DOPJ_STATIC && add_cppflags -DOPJ_STATIC; } }
-enabled libopenmpt        && require_pkg_config libopenmpt "libopenmpt >= 0.2.6557" libopenmpt/libopenmpt.h openmpt_module_create -lstdc++ && append libopenmpt_extralibs "-lstdc++"
+enabled libopenmpt        && require_pkg_config libopenmpt "libopenmpt >= 0.2.6557" libopenmpt/libopenmpt.h openmpt_module_create
 enabled libopenvino       && { { check_pkg_config libopenvino openvino openvino/c/openvino.h ov_core_create && enable openvino2; } ||
                                 { check_pkg_config libopenvino openvino c_api/ie_c_api.h ie_c_api_version ||
                                   require libopenvino c_api/ie_c_api.h ie_c_api_version -linference_engine_c_api; } }
@@ -7369,8 +7370,8 @@ enabled libshaderc        && require_pkg_config spirv_library "shaderc >= 2019.1
 enabled libshine          && require_pkg_config libshine shine shine/layer3.h shine_encode_buffer
 enabled libsmbclient      && { check_pkg_config libsmbclient smbclient libsmbclient.h smbc_init ||
                                require libsmbclient libsmbclient.h smbc_init -lsmbclient; }
-enabled libsnappy         && require libsnappy snappy-c.h snappy_compress -lsnappy -lstdc++
-enabled libsoxr           && require libsoxr soxr.h soxr_create -lsoxr
+enabled libsnappy         && require_pkg_config libsnappy snappy snappy-c.h snappy_compress
+enabled libsoxr           && require libsoxr soxr.h soxr_create -lsoxr $libm_extralibs
 enabled libssh            && require_pkg_config libssh "libssh >= 0.6.0" libssh/sftp.h sftp_init
 enabled libspeex          && require_pkg_config libspeex speex speex/speex.h speex_decoder_init
 enabled libsrt            && require_pkg_config libsrt "srt >= 1.3.0" srt/srt.h srt_socket
@@ -7470,6 +7471,8 @@ enabled openal            && { check_pkg_config openal "openal >= 1.1" "AL/al.h"
 enabled opencl            && { check_pkg_config opencl OpenCL CL/cl.h clEnqueueNDRangeKernel ||
                                check_lib opencl OpenCL/cl.h clEnqueueNDRangeKernel "-framework OpenCL" ||
                                check_lib opencl CL/cl.h clEnqueueNDRangeKernel -lOpenCL ||
+                               check_lib opencl CL/cl.h clEnqueueNDRangeKernel -lOpenCL -lAdvapi32 -lOle32 -lCfgmgr32||
+                               check_lib opencl CL/cl.h clEnqueueNDRangeKernel -lOpenCL -pthread -ldl ||
                                die "ERROR: opencl not found"; } &&
                              { test_cpp_condition "OpenCL/cl.h" "defined(CL_VERSION_1_2)" ||
                                test_cpp_condition "CL/cl.h" "defined(CL_VERSION_1_2)" ||
@@ -7838,7 +7841,7 @@ enabled amf &&
 # Funny iconv installations are not unusual, so check it after all flags have been set
-if enabled libc_iconv; then
+if enabled libc_iconv && disabled iconv; then
     check_func_headers iconv.h iconv
 elif enabled iconv; then
-    check_func_headers iconv.h iconv || check_lib iconv iconv.h iconv -liconv
+    check_func_headers iconv.h iconv || check_lib iconv iconv.h iconv -liconv || check_lib iconv iconv.h iconv -liconv -lcharset
 fi

 enabled debug && add_allcflags -g"$debuglevel" && add_asflags -g"$debuglevel"
'''
BASELINE_PATCH = BASELINE_PATCH.replace(b"@@ -7184,7 +7184,7 @@ fi\n\n-enabled", b"@@ -7184,7 +7184,7 @@ fi\n \n-enabled")
BASELINE_PATCH = BASELINE_PATCH.replace(b"-llzma\n\n enabled zlib && test_exec", b"-llzma\n \n enabled zlib && test_exec")
BASELINE_PATCH = BASELINE_PATCH.replace(b" fi\n\n enabled libmodplug", b" fi\n \n enabled libmodplug")
BASELINE_PATCH = BASELINE_PATCH.replace(b" fi\n\n enabled debug", b" fi\n \n enabled debug")


class BundleTests(unittest.TestCase):
    def test_archive_material_integrity_and_determinism(self):
        # Synthetic 0045 baseline; authentic 0004/0007 fixtures stay unchanged.
        synthetic_hash = hashlib.sha256(dependency_test.BIN2C_BASELINE_PATCH.encode()).hexdigest()
        pin_patch = patch.dict(bundle.BIN2C_PATCH, sha256=synthetic_hash)
        pin_patch.start()
        self.addCleanup(pin_patch.stop)
        for pin in (bundle.MSVC_UTF8_PATCH, bundle.patch_validator.MSVC_UTF8_PIN):
            override = patch.dict(pin, sha256=hashlib.sha256(dependency_test.MSVC_UTF8_BASELINE_PATCH).hexdigest())
            override.start()
            self.addCleanup(override.stop)
        self.assertEqual(hashlib.sha256(BASELINE_PATCH).hexdigest(), bundle.DEPENDENCY_PATCH["sha256"])
        with tempfile.TemporaryDirectory() as temp:
            patch_path = Path(temp) / dependency_test.patcher.PIN["name"]
            for patch_bytes in (BASELINE_PATCH, BASELINE_PATCH.replace(b"\n", b"\r\n")):
                patch_path.write_bytes(patch_bytes)
                self.assertEqual(dependency_test.patcher.validate_baseline_patch(patch_path),
                                 dependency_test.patcher.PIN["sha256"])
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo = root / "repo"
            repo.mkdir()
            def git(*args):
                return bundle.git(repo, *args)
            git("init", "-q")
            # Exercise Git's Windows-style line-ending setting on every host.
            git("config", "core.autocrlf", "true")
            for name, data in {
                "upstream.c": b"pristine source\n",
                "configure": dependency_test.MSVC_UTF8_FIXTURE,
                "ports/ffmpeg/portfile.cmake": PORT,
                "ports/ffmpeg/vcpkg.json": b'{}\n',
                "ports/ffmpeg/z-first.patch": b"first\n",
                "ports/ffmpeg/0004-dependencies.patch": BASELINE_PATCH,
                "ports/ffmpeg/0005-fix-nasm.patch": b"nasm\n",
                "ports/ffmpeg/0007-fix-lib-naming.patch": LIB_NAMING_BASELINE_PATCH,
                "ports/ffmpeg/0045-use-prebuilt-bin2c.patch": dependency_test.BIN2C_BASELINE_PATCH.encode(),
                "ports/ffmpeg/0047-fix-msvc-utf8.patch": dependency_test.MSVC_UTF8_BASELINE_PATCH,
                "ports/ffmpeg/a-second.patch": b"second\n",
                bundle.HELPER: b"helper\n",
            }.items():
                bundle.put(repo, name, data)
            git("add", ".")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                "commit", "-qm", "Fixture")
            commit = git("rev-parse", "HEAD").decode().strip()
            committed_source = git("show", f"{commit}:upstream.c")
            overlay = root / "overlay"
            shutil.copytree(repo / "ports/ffmpeg", overlay)
            # A patched checkout must not leak into the pristine archive.
            (repo / "upstream.c").write_bytes(b"patched source\n")
            overlay_port = overlay / "portfile.cmake"
            overlay_text = overlay_port.read_text().replace("vcpkg_from_github(", "vcpkg_from_git(")
            overlay_text = overlay_text.replace('"0004-dependencies.patch"', '"0004-dependencies-pinned.patch"')
            overlay_text = overlay_text.replace('0007-fix-lib-naming.patch', '0007-fix-lib-naming-pinned.patch')
            overlay_text = overlay_text.replace("0045-use-prebuilt-bin2c.patch", "0045-use-prebuilt-bin2c-pinned.patch")
            overlay_text = overlay_text.replace("        0047-fix-msvc-utf8.patch # superseded upstream\n", "")
            (overlay / "0047-fix-msvc-utf8.patch").unlink()
            overlay_port.write_text(overlay_text)
            adapted = dependency_test.patcher.patch_text(
                dependency_test.FIXTURE,
                dependency_test.patcher.adapt_configure(dependency_test.FIXTURE),
            )
            (overlay / "0004-dependencies-pinned.patch").write_text(adapted)
            adapted_lib_naming = dependency_test.patcher.patch_text(
                dependency_test.LIB_NAMING_FIXTURE,
                dependency_test.patcher.adapt_lib_naming_configure(dependency_test.LIB_NAMING_FIXTURE),
            )
            (overlay / "0007-fix-lib-naming-pinned.patch").write_text(adapted_lib_naming)
            source = root / "common.mak"
            # Preserve the pinned LF fixture bytes on Windows too.
            source.write_bytes(dependency_test.BIN2C_FIXTURE.encode("utf-8"))
            with patch.dict(dependency_test.patcher.BIN2C_PIN, sha256=synthetic_hash):
                dependency_test.patcher.generate_bin2c(
                    source, repo / "ports/ffmpeg/0045-use-prebuilt-bin2c.patch",
                    overlay / "0045-use-prebuilt-bin2c-pinned.patch")
            with patch.multiple(bundle, COMMIT=commit, BASELINE=commit, ARCHIVE=f"ffmpeg-{commit}.tar"):
                first, second = root / "first", root / "second"
                bundle.create(repo, repo, overlay, first)
                bundle.create(repo, repo, overlay, second)
                expected = bundle.record(first / "MANIFEST.json")
                self.assertEqual(expected, bundle.record(second / "MANIFEST.json"))
                with tarfile.open(first / bundle.ARCHIVE) as archive:
                    self.assertEqual(archive.extractfile(f"ffmpeg-{commit}/upstream.c").read(), committed_source)
                    self.assertEqual(archive.pax_headers["comment"], commit)
                staged = root / "app-doc/ffmpeg-source-bundle"
                bundle.stage(first, staged, expected)
                self.assertEqual(expected, bundle.record(staged / "MANIFEST.json"))
                manifest = json.loads((first / "MANIFEST.json").read_text())
                self.assertIn("# retained inline comment", manifest["patches"]["baseline"]["block"])
                self.assertEqual([Path(p).name for p in manifest["patches"]["baseline"]["ordered_files"]],
                                 ["z-first.patch", "0004-dependencies.patch", "0005-fix-nasm.patch",
                                  "0007-fix-lib-naming.patch", "0045-use-prebuilt-bin2c.patch", "0047-fix-msvc-utf8.patch", "a-second.patch"])
                self.assertEqual([Path(p).name for p in manifest["patches"]["overlay"]["ordered_files"]],
                                 ["z-first.patch", "0004-dependencies-pinned.patch", "0005-fix-nasm.patch",
                                  "0007-fix-lib-naming-pinned.patch", "0045-use-prebuilt-bin2c-pinned.patch", "a-second.patch"])
                self.assertEqual(manifest["schema"], 4)
                self.assertEqual(len(manifest["patches"]["adaptations"]), 3)
                self.assertEqual({item["kind"] for item in manifest["patches"]["adaptations"]},
                                 {"exact-context pinned-source adaptation"})
                self.assertEqual(len(manifest["patches"]["supersessions"]), 1)
                self.assertEqual(manifest["patches"]["supersessions"][0]["source_sha256"],
                                 hashlib.sha256(dependency_test.MSVC_UTF8_FIXTURE).hexdigest())
                # Hash/size mismatches and missing inputs must all stop staging.
                for name in [bundle.ARCHIVE, "baseline/ports/ffmpeg/0004-dependencies.patch",
                             "baseline/ports/ffmpeg/0007-fix-lib-naming.patch",
                             "baseline/ports/ffmpeg/0045-use-prebuilt-bin2c.patch",
                             "baseline/ports/ffmpeg/0047-fix-msvc-utf8.patch",
                             "overlay/ffmpeg/0004-dependencies-pinned.patch",
                             "overlay/ffmpeg/0007-fix-lib-naming-pinned.patch",
                             "overlay/ffmpeg/0045-use-prebuilt-bin2c-pinned.patch",
                             "overlay/ffmpeg/portfile.cmake", "build-inputs/pins.json", "MANIFEST.json"]:
                    path = first / name
                    original = path.read_bytes()
                    for data in [None, original + b"corrupt"]:
                        if data is None:
                            path.unlink()
                        else:
                            path.write_bytes(data)
                        with self.assertRaises((ValueError, FileNotFoundError)):
                            bundle.stage(first, root / "must-not-stage", expected)
                        self.assertFalse((root / "must-not-stage").exists())
                        path.write_bytes(original)
                # Even an internally rehashed manifest cannot omit required
                # source or rewrite either patch order.
                for name in [bundle.ARCHIVE, "baseline/ports/ffmpeg/0004-dependencies.patch",
                             "baseline/ports/ffmpeg/0007-fix-lib-naming.patch"]:
                    changed = json.loads(json.dumps(manifest))
                    del changed["files"][name]
                    (first / "MANIFEST.json").write_text(json.dumps(changed))
                    with self.assertRaises(ValueError):
                        bundle.verify(first, bundle.record(first / "MANIFEST.json"))
                changed = json.loads(json.dumps(manifest))
                changed["patches"]["overlay"]["ordered_files"].reverse()
                (first / "MANIFEST.json").write_text(json.dumps(changed))
                with self.assertRaises(ValueError):
                    bundle.verify(first, bundle.record(first / "MANIFEST.json"))

                # Rehashed evidence, baseline, overlay or source must not turn
                # an omission into a pass without rechecking its exact proof.
                def verify_changed(changed):
                    (first / "MANIFEST.json").write_text(json.dumps(changed))
                    with self.assertRaises(ValueError):
                        bundle.verify(first, bundle.record(first / "MANIFEST.json"))
                for key in ("baseline_sha256", "source_sha256", "contexts_sha256", "kind", "ffmpeg_commit"):
                    changed = json.loads(json.dumps(manifest))
                    changed["patches"]["supersessions"][0][key] = "wrong"
                    verify_changed(changed)
                for name, data in (
                    ("overlay/ffmpeg/0047-fix-msvc-utf8.patch", dependency_test.MSVC_UTF8_BASELINE_PATCH),
                    ("baseline/ports/ffmpeg/0047-fix-msvc-utf8.patch", b"changed patch"),
                    ("overlay/ffmpeg/portfile.cmake", overlay_port.read_bytes().replace(b"        a-second.patch", b"        0047-fix-msvc-utf8.patch\n        a-second.patch")),
                ):
                    path = first / name
                    original = path.read_bytes() if path.exists() else None
                    path.write_bytes(data)
                    changed = json.loads(json.dumps(manifest))
                    changed["files"][name] = bundle.record(path)
                    verify_changed(changed)
                    if original is None:
                        path.unlink()
                    else:
                        path.write_bytes(original)
                archive_path = first / bundle.ARCHIVE
                original = archive_path.read_bytes()
                for source in (dependency_test.MSVC_UTF8_FIXTURE.replace(b"check_allcflags -utf-8", b"check_cflags -utf-8"),
                               dependency_test.MSVC_UTF8_FIXTURE * 2,
                               dependency_test.MSVC_UTF8_FIXTURE + b"# changed source identity\n"):
                    with tarfile.open(archive_path, "w") as archive:
                        member = tarfile.TarInfo(f"ffmpeg-{commit}/configure")
                        member.size = len(source)
                        archive.addfile(member, io.BytesIO(source))
                    changed = json.loads(json.dumps(manifest))
                    changed["files"][bundle.ARCHIVE] = bundle.record(archive_path)
                    verify_changed(changed)
                for copies in (0, 2):
                    with tarfile.open(archive_path, "w") as archive:
                        for _ in range(copies):
                            member = tarfile.TarInfo(f"ffmpeg-{commit}/configure")
                            member.size = len(dependency_test.MSVC_UTF8_FIXTURE)
                            archive.addfile(member, io.BytesIO(dependency_test.MSVC_UTF8_FIXTURE))
                    changed = json.loads(json.dumps(manifest))
                    changed["files"][bundle.ARCHIVE] = bundle.record(archive_path)
                    verify_changed(changed)
                archive_path.write_bytes(original)

    def test_lib_naming_bundle_patch_is_additive_only(self):
        original = dependency_test.LIB_NAMING_FIXTURE
        adapted = dependency_test.patcher.adapt_lib_naming_configure(original)
        patch_text = dependency_test.patcher.patch_text(original, adapted)
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / bundle.LIB_NAMING_PATCH["overlay_name"]
            path.write_text(patch_text)
            bundle.validate_adapted_patch(path, bundle.LIB_NAMING_PATCH)
            path.write_text(patch_text.replace(
                "+            -lmp3lame)",
                "-            -lz)                  echo ${zlib_msvc_lib:-zlib.lib} ;;\n+            -lmp3lame)"))
            with self.assertRaisesRegex(ValueError, "only add the mp3lame mapping"):
                bundle.validate_adapted_patch(path, bundle.LIB_NAMING_PATCH)

    def test_mirror_pin_and_fetch_guards(self):
        pins = json.loads((ROOT / "pins.json").read_text())["ffmpeg"]
        self.assertEqual(pins["repository"], "https://github.com/FFmpeg/FFmpeg.git")
        self.assertEqual(pins["commit"], "db8537d8cc7962bd3d07254d6ceff21d888817d4")
        build = (ROOT / "build-windows-ffmpeg-prototype.ps1").read_text()
        overlay = (ROOT / "derive-ffmpeg-overlay.ps1").read_text()
        self.assertIn(f'if ($ffmpegRepo -ne "{pins["repository"]}") {{ throw', build)
        self.assertIn(f'if ($ffmpegCommit -ne "{pins["commit"]}") {{ throw', build)
        self.assertIn(f"$Repository -ne '{pins['repository']}'", overlay)
        # The mirror changes transport only: each Git failure must stop before
        # patches/builds, and a detached checkout must match the immutable pin.
        for command, failure in (
            ('git clone --no-checkout $ffmpegRepo $workDir', 'FFmpeg clone failed'),
            ('git fetch --depth 1 origin $ffmpegCommit', 'FFmpeg fetch of pinned commit $ffmpegCommit failed'),
            ('git cat-file -e "$ffmpegCommit^{commit}"', 'pinned FFmpeg commit object is missing after fetch'),
            ('git checkout --detach $ffmpegCommit --quiet', 'FFmpeg checkout of $ffmpegCommit failed'),
        ):
            self.assertIn(command + '\nif ($LASTEXITCODE -ne 0) { throw "' + failure + '" }',
                          build.replace('    ', ''))
        self.assertIn('$actual = ((git rev-parse HEAD) | Out-String).Trim()', build)
        self.assertIn('if ($actual -ne $ffmpegCommit) { throw', build)

    def test_package_and_evidence_wiring(self):
        build = (ROOT / "build-windows-ffmpeg-prototype.ps1").read_text()
        stage = (ROOT / "stage-windows-prototype-app.ps1").read_text()
        workflow = (ROOT.parent.parent / ".github/workflows/windows-ffmpeg-prototype.yml").read_text()
        self.assertLess(build.index('source-bundle.py") create'), build.index('& $vcpkgExe install'))
        self.assertIn('source_bundle_manifest = $sourceBundleRecord', build)
        self.assertIn('ffmpeg-dependency-patch.py") $adaptation.command', build)
        self.assertIn('adapt-lib-naming', build)
        self.assertIn('Set-PrototypeFfmpegPatch', build)
        self.assertIn('$bundleRecord = $build.source_bundle_manifest', stage)
        self.assertIn('source-bundle.py") stage', stage)
        self.assertIn('(Join-Path $docDir "ffmpeg-source-bundle")', stage)
        self.assertIn('if ($LASTEXITCODE -ne 0) { throw "refusing to stage: source bundle', stage)
        self.assertIn('test-prototype-audit-report.py', workflow)
        self.assertIn('create-prototype-audit-report.py', workflow)
        self.assertIn('IRIS_PROTOTYPE_AUDIT_REPORT_MAX_BYTES: "262144"', workflow)
        self.assertIn('path: target/prototype-audit-report.json', workflow)
        self.assertIn('if-no-files-found: error', workflow)
        for forbidden in (
            'target/prototype-windows-ffmpeg/**',
            'target/prototype-windows-pdfium/**',
            'target/prototype-windows-app/**',
            'target/prototype-windows-app/doc/ffmpeg-source-bundle/**',
            'target/prototype-windows-installer/**',
        ):
            self.assertNotIn(forbidden, workflow)
        self.assertIn('test-source-bundle.py', workflow)
        self.assertIn('test-ffmpeg-dependency-patch.py', workflow)
        self.assertIn('Flags: recursesubdirs', (ROOT / "windows-prototype-installer.iss").read_text())


if __name__ == "__main__":
    unittest.main()
