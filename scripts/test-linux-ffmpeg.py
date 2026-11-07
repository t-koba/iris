#!/usr/bin/env python3
"""Offline contracts: no source download, native compilation or package install."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("ffmpeg", ROOT / "scripts/linux-ffmpeg.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class RuntimeContracts(unittest.TestCase):
    def test_pinned_signature_identity(self):
        fingerprint = module.POLICY["fingerprint"]
        valid = f"[GNUPG:] VALIDSIG {fingerprint} 2026-01-01 1 0 4 0 1 10 00 {fingerprint}\n"
        module.signature(valid)
        module.signature(valid.replace(fingerprint, "A" * 40, 1))  # bound signing subkey
        for bad in ("", "[GNUPG:] VALIDSIG\n", valid.replace(fingerprint, "A" * 40),
                    valid + "[GNUPG:] REVKEYSIG key name\n", valid + valid):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                module.signature(bad)
        policy = module.POLICY
        self.assertEqual(policy["system_packages"]["libgcc-s1"],
                         "GPL-3.0-or-later WITH GCC-exception-3.1 (GCC runtime library exception; see distro copyright)")
        self.assertEqual(policy["sha256"], "7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3")
        self.assertEqual(policy["avcodec_version"], [62, 28, 103])
        self.assertEqual(policy["url"], "https://ffmpeg.org/releases/ffmpeg-8.1.3.tar.xz")

    def test_generated_config_fails_closed(self):
        text = "\n".join(f"#define CONFIG_{k} {v}" for k, v in
                         dict(GPL=0, VERSION3=0, NONFREE=0, SHARED=1, STATIC=0).items())
        module.config(text)
        for line in text.splitlines():
            bad = text.replace(line, line[:-1] + str(1 - int(line[-1])))
            with self.subTest(line=line), self.assertRaises(ValueError):
                module.config(bad)
        with self.assertRaises(ValueError):
            module.config("")

    def test_compressed_builtin_decoders_are_retained(self):
        text = "\n".join(f"#define CONFIG_{name} 1" for name in module.POLICY["required_decoders"])
        self.assertEqual(module.codecs(text), module.POLICY["required_decoders"])
        with self.assertRaisesRegex(ValueError, "built-in decoder"):
            module.codecs(text.replace("CONFIG_PNG_DECODER 1", "CONFIG_PNG_DECODER 0"))
        for name in ("zlib", "bzlib", "lzma", "iconv"):
            self.assertIn(f"--enable-{name}", module.POLICY["configure_flags"])
        self.assertFalse(any(flag.startswith(("--disable-decoder", "--disable-everything"))
                             for flag in module.POLICY["configure_flags"]))

    def test_library_inventory_and_symlink_ownership(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "libavcodec.so.62.28.103").write_bytes(b"exact shared library")
            (path / "libavcodec.so.62").symlink_to("libavcodec.so.62.28.103")
            before = module.inventory(path)
            self.assertEqual(before["libavcodec.so.62"], {"target": "libavcodec.so.62.28.103"})
            (path / "libavcodec.so.62.28.103").write_bytes(b"different bytes")
            self.assertNotEqual(before, module.inventory(path))
            (path / "libavcodec.so").symlink_to("../elsewhere")
            with self.assertRaises(ValueError):
                module.inventory(path)

    def test_elf_resolution_parser_and_system_provenance(self):
        text = """linux-vdso.so.1 (0x1)
  libavcodec.so.62 => /private/libavcodec.so.62 (0x2)
  libc.so.6 => /lib/libc.so.6 (0x3)
  /lib64/ld-linux-x86-64.so.2 (0x4)"""
        with patch.object(module, "run", return_value=text) as command:
            resolved = module.resolution(Path("Iris"))
            command.assert_called_once_with("ldd", "Iris")
            self.assertEqual(resolved["ld-linux-x86-64.so.2"], "/lib64/ld-linux-x86-64.so.2")
            command.return_value = "libavcodec.so.62 => not found"
            with self.assertRaisesRegex(ValueError, "unresolved"):
                module.resolution(Path("Iris"))
        with patch.object(module, "run", return_value="unreviewed-package: /lib/libc.so.6"):
            with self.assertRaisesRegex(ValueError, "unreviewed system library package"):
                module.system_dependency("/lib/libc.so.6")

    def test_native_abi_license_flags_and_dependency_closure(self):
        class Function:
            def __init__(self, result):
                self.result = result
            def __call__(self):
                return self.result
        class Library:
            def __getattr__(self, attr):
                if attr.endswith("_configuration"):
                    return Function(flags.encode())
                if attr.endswith("_license"):
                    return Function(license_text.encode())
                return Function(abi)
        flags = " ".join(module.POLICY["configure_flags"])
        license_text = "LGPL version 2.1 or later"
        abi = (62 << 16) | (28 << 8) | 103
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            for name in module.POLICY["libraries"]:
                (path / f"lib{name}.so").write_bytes(b"mock ELF")
            with patch.object(module, "soname", side_effect=lambda p: p.name + ".62"), \
                 patch.object(module.ctypes, "CDLL", return_value=Library()), \
                 patch.object(module, "system_dependency", return_value={"package": "libc6"}), \
                 patch.object(module, "resolution", return_value={"libc.so.6": "/lib/libc.so.6"}) as deps:
                self.assertEqual(len(module.runtime(path)), 7)
                flags += " --enable-gpl"
                with self.assertRaisesRegex(ValueError, "build flags"):
                    module.runtime(path)
                flags = " ".join(module.POLICY["configure_flags"])
                license_text = "GPL version 2 or later"
                with self.assertRaisesRegex(ValueError, "license"):
                    module.runtime(path)
                license_text = "LGPL version 2.1 or later"
                abi -= 1
                with self.assertRaisesRegex(ValueError, "ABI"):
                    module.runtime(path)
                abi += 1
                deps.return_value = {"libx264.so.1": "/lib/libx264.so.1"}
                with self.assertRaisesRegex(ValueError, "unreviewed"):
                    module.runtime(path)
                deps.return_value = {"libavutil.so.60": "/lib/libavutil.so.60"}
                with self.assertRaisesRegex(ValueError, "system FFmpeg"):
                    module.runtime(path)

    def test_actual_product_resolution_and_report_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            libdir = root / "usr/lib/iris"
            libdir.mkdir(parents=True)
            docs = root / "usr/share/doc/iris"
            docs.mkdir(parents=True)
            (docs / "system-dependencies.txt").write_text("libc6 (>= 2.39)\n")
            binary = root / "usr/bin/Iris"
            binary.parent.mkdir()
            binary.write_bytes(b"Iris")
            names = ["libavcodec.so.62", "libavformat.so.62"]
            for name in names:
                (libdir / name).write_bytes(name.encode())
            manifest = {"libraries": dict.fromkeys(names)}
            closure = {n: str(libdir / n) for n in names}
            output = root / "audit.json"
            package = {"package_identity": ["libgcc-s1:amd64", "14.2.0-4ubuntu2~24.04.1", "gcc-14", "14.2.0-4ubuntu2~24.04.1"],
                       "sha256": "a" * 64,
                       "license_reference": module.POLICY["system_packages"]["libgcc-s1"],
                       "copyright_sha256": "b" * 64}
            with patch.object(module, "validate", return_value=manifest), \
                 patch.object(module, "run", return_value="exact-source-sha"), \
                 patch.object(module, "resolution", return_value=closure), \
                 patch.object(module, "system_dependency", return_value=package) as provenance:
                closure["libgcc_s.so.1"] = "/usr/lib/x86_64-linux-gnu/libgcc_s.so.1"
                module.check(root, output, 4096)
                report = json.loads(output.read_text())
                self.assertEqual(report["source_sha"], "exact-source-sha")
                self.assertEqual(report["system_product_dependencies"], {"libgcc_s.so.1": package})
                provenance.assert_called_once_with("/usr/lib/x86_64-linux-gnu/libgcc_s.so.1")
                output.unlink()
                provenance.side_effect = ValueError("unreviewed system library package")
                with self.assertRaisesRegex(ValueError, "unreviewed system library package"):
                    module.check(root, output, 4096)
                self.assertFalse(output.exists())
                provenance.side_effect = None
                with self.assertRaisesRegex(ValueError, "byte budget"):
                    module.check(root, output, 1)
                self.assertFalse(output.exists())
                closure[names[0]] = "/usr/lib/" + names[0]
                with self.assertRaisesRegex(ValueError, "system FFmpeg"):
                    module.check(root, output, 4096)

    def test_build_staging_and_workflow_contract(self):
        workflow = (ROOT / ".github/workflows/linux-ffmpeg-validation.yml").read_text()
        # This reusable workflow has one call trigger and one bounded JSON upload.
        triggers = workflow.split("\non:\n", 1)[1].split("\npermissions:", 1)[0]
        self.assertEqual(triggers.strip(), "workflow_call:")
        self.assertNotIn("workflow_dispatch:", workflow)
        self.assertNotIn("inputs:", workflow)
        self.assertNotIn("release.yml", workflow)
        self.assertNotIn("GH_TOKEN", workflow)
        self.assertIn("ref: ${{ github.sha }}", workflow)
        self.assertIn("toolchain: 1.97.1", workflow)
        self.assertIn('IRIS_MAX_LINUX_RELEASE_BYTES: "35651584"', workflow)
        self.assertIn("vars.IRIS_FFMPEG_MAX_REPORT_BYTES || 266240", workflow)
        self.assertIn('IRIS_FFMPEG_MAX_REPORT_BYTES", "266240"', (ROOT / "scripts/linux-ffmpeg.py").read_text())
        self.assertEqual(workflow.count("uses: actions/upload-artifact@"), 1)
        upload = workflow.split("uses: actions/upload-artifact@", 1)[1]
        self.assertEqual(upload.split("path:", 1)[1].strip(), "audit/linux-ffmpeg.json")
        self.assertIn("--locked --offline", workflow)
        self.assertIn("staged == installed", workflow)
        smoke = (ROOT / "scripts/smoke-linux-deb.sh").read_text()
        self.assertIn("find /usr/lib/iris -mindepth 1", smoke)
        self.assertIn("extract_video_thumbnail", (ROOT / "crates/thumb-preview/examples/video_thumbnail_smoke.rs").read_text())
        build = (ROOT / "scripts/build-linux-ffmpeg.sh").read_text()
        self.assertLess(build.index('get sha256'), build.index('tar -xJf'))
        self.assertLess(build.index('--verify'), build.index('tar -xJf'))
        self.assertIn('get fingerprint', build)
        package = (ROOT / "scripts/package-linux-appdir.sh").read_text()
        self.assertIn('PKG_CONFIG_LIBDIR="$IRIS_FFMPEG_PREFIX/lib/pkgconfig"', package)
        self.assertIn('linux-ffmpeg.py" stage', package)
        self.assertIn('dpkg-shlibdeps -O -xiris', package)
        self.assertNotIn('--ignore-missing-info', package.split('cat > "$deb_build_dir/debian/control"')[1])
        release_linux = (ROOT / ".github/workflows/release.yml").read_text().split('  package-linux:', 1)[1].split('  package-windows:', 1)[0]
        self.assertNotIn('libavcodec-dev', release_linux)
        self.assertNotIn('clang ffmpeg', release_linux)


if __name__ == "__main__":
    unittest.main()
