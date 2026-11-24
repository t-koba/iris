#!/usr/bin/env python3
"""Offline contracts: no source download, native compilation or package install."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
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
        flags = ("--prefix=/example/build --libdir=/example/build/lib "
                 + " ".join(module.POLICY["configure_flags"]))
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
                records = module.runtime(path)
                self.assertEqual(len(records), 7)
                self.assertEqual(records[0]["configuration"], module.POLICY["configure_flags"])
                self.assertNotIn("/example/build", json.dumps(records))
                flags += " --enable-gpl"
                with self.assertRaisesRegex(ValueError, "build flags") as raised:
                    module.runtime(path)
                self.assertNotIn("/example/build", str(raised.exception))
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

    def test_pinned_fetch_retries_are_bounded_and_configurable(self):
        build = (ROOT / "scripts/build-linux-ffmpeg.sh").read_text()
        self.assertIn('fetch_seconds=${IRIS_FFMPEG_FETCH_SECONDS:-300}', build)
        self.assertIn('fetch_retries=${IRIS_FFMPEG_FETCH_RETRIES:-1}', build)
        self.assertIn('fetch_retry_delay=${IRIS_FFMPEG_FETCH_RETRY_DELAY_SECONDS:-2}', build)
        self.assertIn('--max-time "$fetch_seconds"', build)
        self.assertIn('--retry-all-errors --retry "$fetch_retries"', build)
        self.assertIn('--retry-delay "$fetch_retry_delay"', build)
        readme = (ROOT / "packaging/linux/ffmpeg-README.md").read_text()
        self.assertIn("`0` is passed", readme)
        self.assertIn("selects its default exponential backoff, not immediate retries.", readme)
        self.assertIn('echo "$(get sha256)', build)
        self.assertIn('--verify "$work/source.tar.xz.asc" "$work/source.tar.xz"', build)
        for workflow in ("ci.yml", "linux-ffmpeg-validation.yml", "release.yml"):
            contents = (ROOT / ".github/workflows" / workflow).read_text()
            self.assertIn('vars.IRIS_FFMPEG_FETCH_SECONDS || 300', contents)
            self.assertIn('vars.IRIS_FFMPEG_FETCH_RETRIES || 1', contents)
            self.assertIn('vars.IRIS_FFMPEG_FETCH_RETRY_DELAY_SECONDS || 2', contents)

    def test_pdfium_fetch_retry_validation_and_arguments(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "bin"
            tools.mkdir()
            log = root / "curl-args"
            # No network or archive extraction: record argv, then simulate exhaustion.
            for name, body in (("curl", 'printf "%s\\n" "$@" > "$PDFIUM_TEST_CURL_ARGS"; exit 22'),
                               ("uname", 'echo x86_64')):
                tool = tools / name
                tool.write_text("#!/bin/sh\n" + body + "\n")
                tool.chmod(0o755)
            env = {k: v for k, v in os.environ.items() if not k.startswith("IRIS_PDFIUM_")}
            env.update(PATH=str(tools) + os.pathsep + os.environ["PATH"],
                       IRIS_PDFIUM_CACHE_DIR=str(root / "cache"),
                       PDFIUM_TEST_CURL_ARGS=str(log))
            retries = "IRIS_PDFIUM_FETCH_RETRIES"
            delay = "IRIS_PDFIUM_FETCH_RETRY_DELAY_SECONDS"
            def run(settings):
                log.unlink(missing_ok=True)
                result = subprocess.run(
                    ["bash", str(ROOT / "scripts/fetch-pdfium-runtime.sh"), str(root / "AppDir")],
                    env=dict(env, **settings), capture_output=True, text=True, timeout=5)
                self.assertFalse((root / "AppDir").exists())
                return result
            for name in (retries, delay):
                for value in ("-1", "1.5", "abc", "1 2", "+1"):
                    with self.subTest(name=name, value=value):
                        result = run({name: value})
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn(name + " must be non-negative", result.stderr)
                        self.assertFalse(log.exists())
                        self.assertFalse((root / "cache").exists())
            for settings, count, seconds in (({}, "3", "2"),
                                              ({retries: "", delay: ""}, "3", "2"),
                                              ({retries: "0", delay: "0"}, "0", "0"),
                                              ({retries: "7", delay: "4"}, "7", "4")):
                with self.subTest(settings=settings):
                    self.assertEqual(run(settings).returncode, 22)
                    self.assertEqual(log.read_text().splitlines(), [
                        "--fail", "--location", "--silent", "--show-error",
                        "--retry", count, "--retry-delay", seconds,
                        "--proto", "=https", "--tlsv1.2", "--output",
                        str(root / "cache/pdfium-linux-x64.tgz.part"),
                        "https://github.com/bblanchon/pdfium-binaries/releases/download/"
                        "chromium/8138/pdfium-linux-x64.tgz"])

    def test_pdfium_attestation_status_tracks_staged_archive_verification(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "bin"
            tools.mkdir()
            # Mock downloads, upstream hashes, extraction and gh: synthetic bytes
            # never leave this temporary directory. Staged library hashing is real.
            tarball = root / "pdfium-linux-x64.tgz"
            with tarball.open("wb") as stream:
                stream.truncate(3945444)
            real_sha256sum = shutil.which("sha256sum")
            self.assertIsNotNone(real_sha256sum)
            asset_sha256 = "d6b1e28950a0392590d79bbdbb7eb19686872b688f8a36f170acb19e41473113"

            scripts = {
                "uname": "#!/bin/sh\necho x86_64\n",
                "sha256sum": '''#!/bin/sh
if [ "$1" = "$IRIS_PDFIUM_TARBALL" ]; then
    printf '%s  %s\\n' "$PDFIUM_TEST_ASSET_SHA256" "$1"
elif [ "${1##*/}" = pdfium-attestation.json ]; then
    printf '%s  %s\\n' "$PDFIUM_TEST_BUNDLE_SHA256" "$1"
elif [ "$1" = "$PDFIUM_TEST_GH_EXECUTABLE" ] && [ "$PDFIUM_TEST_GH_HASH" != real ]; then
    printf '%s\\n' "$PDFIUM_TEST_GH_HASH"
    exit "$PDFIUM_TEST_GH_HASH_EXIT"
else
    exec "$PDFIUM_TEST_REAL_SHA256SUM" "$@"
fi
''',
                "realpath": '''#!/bin/sh
[ "$PDFIUM_TEST_REALPATH_EXIT" = 0 ] || exit "$PDFIUM_TEST_REALPATH_EXIT"
exec "$PDFIUM_TEST_REAL_REALPATH" "$@"
''',
                "curl": '''#!/bin/sh
printf '%s\\n' "$@" > "$PDFIUM_TEST_CURL_ARGS"
output=
while [ "$#" -gt 0 ]; do
    if [ "$1" = --output ]; then shift; output=$1; fi
    shift
done
[ -n "$output" ] || exit 2
truncate -s "$PDFIUM_TEST_BUNDLE_BYTES" "$output"
exit "${PDFIUM_TEST_CURL_EXIT:-0}"
''',
                "tar": '''#!/bin/sh
extract=
want_extract=0
for arg in "$@"; do
    if [ "$want_extract" = 1 ]; then extract=$arg; want_extract=0; continue; fi
    if [ "$arg" = "-C" ]; then want_extract=1; fi
done
[ -n "$extract" ] || exit 2
mkdir -p "$extract/licenses" "$extract/lib"
printf 'PDFium license test\\n' > "$extract/LICENSE"
printf 'MAJOR=159\\nMINOR=0\\nBUILD=8138\\nPATCH=0\\n' > "$extract/VERSION"
printf 'args\\n' > "$extract/args.gn"
printf 'license test\\n' > "$extract/licenses/pdfium.txt"
printf 'synthetic runtime test bytes\\n' > "$extract/lib/libpdfium.so"
''',
                "gh-real": '''#!/bin/sh
if [ "$1" = version ]; then
    printf '%s\\n' "$0" > "$PDFIUM_TEST_GH_VERSION_ARGS"
    printf '%s\\n' "$PDFIUM_TEST_GH_VERSION"
    # Retarget the PATH symlink to a different executable after version lookup.
    # Verification must still invoke this resolved executable, not the new target.
    ln -sfn "$PDFIUM_TEST_GH_OTHER" "$PDFIUM_TEST_GH_LINK"
    exit "$PDFIUM_TEST_GH_VERSION_EXIT"
fi
printf '%s\\n' "$0" > "$PDFIUM_TEST_GH_INVOKED"
printf '%s\\n' "$@" > "$PDFIUM_TEST_GH_ARGS"
# The verified bundle and exact archive must still exist during verification.
[ -f "$3" ] || exit 2
[ "$6" = --bundle ] && [ -f "$7" ] || exit 2
exit "${PDFIUM_TEST_GH_EXIT:-0}"
''',
            }
            for name, body in scripts.items():
                tool = tools / name
                tool.write_text(body, encoding="utf-8")
                tool.chmod(0o755)

            gh_executable = tools / "gh-real"
            gh_link = tools / "gh"
            gh_other = tools / "gh-other"
            gh_other.write_text("#!/bin/sh\nexit 97\n", encoding="utf-8")
            gh_other.chmod(0o755)
            verifier_sha256 = hashlib.sha256(gh_executable.read_bytes()).hexdigest()
            base_env = {
                key: value for key, value in os.environ.items()
                if not key.startswith("IRIS_PDFIUM_") and key != "GH_TOKEN"
            }
            base_env.update(
                PATH=str(tools) + os.pathsep + os.environ["PATH"],
                IRIS_PDFIUM_TARBALL=str(tarball),
                PDFIUM_TEST_ASSET_SHA256=asset_sha256,
                PDFIUM_TEST_REAL_SHA256SUM=real_sha256sum,
                PDFIUM_TEST_REAL_REALPATH=shutil.which("realpath"),
                PDFIUM_TEST_GH_EXECUTABLE=str(gh_executable),
                PDFIUM_TEST_GH_LINK=str(gh_link),
                PDFIUM_TEST_GH_OTHER=str(gh_other),
            )

            bundle_digest = "7526f4b934ab498d6206ac7f3d78dd4ce8e0f7d3175227ab3f4ecc255d950ad7"
            def run(name, verify, token=False, gh_exit="0", bundle_size="18267",
                    bundle_sha256=bundle_digest, curl_exit="0",
                    gh_version="gh version 2.83.0 (2025-11-04)\nhttps://github.com/cli/cli/releases/tag/v2.83.0",
                    gh_version_exit="0", gh_hash="real", gh_hash_exit="0", realpath_exit="0", gh_target=None):
                gh_link.unlink(missing_ok=True)
                gh_link.symlink_to(gh_target or gh_executable)
                appdir = root / name
                gh_args = root / (name + "-gh-args")
                env = dict(base_env)
                env.update(
                    PDFIUM_TEST_BUNDLE_BYTES=bundle_size,
                    PDFIUM_TEST_BUNDLE_SHA256=bundle_sha256,
                    PDFIUM_TEST_CURL_ARGS=str(root / (name + "-curl-args")),
                    PDFIUM_TEST_CURL_EXIT=curl_exit,
                    IRIS_VERIFY_PDFIUM_PROVENANCE=verify,
                    PDFIUM_TEST_GH_ARGS=str(gh_args),
                    PDFIUM_TEST_GH_EXIT=gh_exit,
                    PDFIUM_TEST_GH_VERSION=gh_version,
                    PDFIUM_TEST_GH_VERSION_EXIT=gh_version_exit,
                    PDFIUM_TEST_GH_HASH=gh_hash,
                    PDFIUM_TEST_GH_HASH_EXIT=gh_hash_exit,
                    PDFIUM_TEST_REALPATH_EXIT=realpath_exit,
                    PDFIUM_TEST_GH_VERSION_ARGS=str(root / (name + "-gh-version-args")),
                    PDFIUM_TEST_GH_INVOKED=str(root / (name + "-gh-invoked")),
                )
                if token:
                    env["GH_TOKEN"] = "test-only"
                result = subprocess.run(
                    ["bash", str(ROOT / "scripts/fetch-pdfium-runtime.sh"), str(appdir)],
                    env=env, capture_output=True, text=True, timeout=5,
                )
                return result, appdir, gh_args

            manual, appdir, gh_args = run("manual", "0", gh_target=root / "absent-gh")
            self.assertEqual(manual.returncode, 0, manual.stderr)
            self.assertFalse(gh_args.exists())
            self.assertFalse((root / "manual-curl-args").exists())
            manual_record = json.loads(
                (appdir / "usr/share/doc/iris/pdfium/PROVENANCE.json").read_text(encoding="utf-8")
            )
            self.assertEqual(manual_record["schema"], 2)
            self.assertEqual(manual_record["attestation"]["status"], "not_attempted")
            self.assertEqual(manual_record["attestation"]["subject_sha256"], asset_sha256)
            self.assertIsNone(manual_record["attestation"]["bundle_sha256"])
            self.assertIsNone(manual_record["attestation"]["bundle_bytes"])
            for key in ("verifier_version", "verifier_sha256"):
                self.assertIsNone(manual_record["attestation"][key])
            self.assertFalse((root / "manual-gh-version-args").exists())
            manual_text = (appdir / "usr/share/doc/iris/pdfium/PROVENANCE").read_text()
            self.assertIn("Attestation verifier gh version: not_attempted", manual_text)
            self.assertIn("Attestation verifier executable sha256: not_attempted", manual_text)

            tagged, appdir, gh_args = run("tagged", "1", token=True)
            self.assertEqual(tagged.returncode, 0, tagged.stderr)
            self.assertEqual((root / "tagged-gh-version-args").read_text().strip(), str(gh_executable))
            self.assertEqual((root / "tagged-gh-invoked").read_text().strip(), str(gh_executable))
            self.assertEqual(gh_link.resolve(), gh_other)
            args = gh_args.read_text(encoding="utf-8").splitlines()
            bundle = Path(args[6])
            commit = "0eb94b28c2252d2baae3dfe15dc6b237131d8cd8"
            workflow = "github.com/bblanchon/pdfium-binaries/.github/workflows/build-all.yml"
            self.assertEqual(args, [
                "attestation", "verify", str(tarball), "--repo", "bblanchon/pdfium-binaries",
                "--bundle", str(bundle), "--signer-workflow", workflow,
                "--signer-digest", commit, "--source-digest", commit,
                "--source-ref", "refs/heads/master",
            ])
            self.assertEqual(bundle.name, "pdfium-attestation.json")
            self.assertFalse(bundle.parent.exists())  # disposable on success
            self.assertEqual((root / "tagged-curl-args").read_text().splitlines(), [
                "--fail", "--location", "--silent", "--show-error", "--retry", "3",
                "--retry-delay", "2", "--proto", "=https", "--tlsv1.2", "--output", str(bundle),
                "https://github.com/bblanchon/pdfium-binaries/releases/download/"
                "chromium/8138/pdfium-attestation.json",
            ])
            tagged_record = json.loads(
                (appdir / "usr/share/doc/iris/pdfium/PROVENANCE.json").read_text(encoding="utf-8")
            )
            self.assertEqual(tagged_record["schema"], 2)
            self.assertEqual(tagged_record["attestation"]["status"], "verified")
            self.assertEqual(tagged_record["attestation"], {
                "status": "verified", "pin_sha256": bundle_digest,
                "verifier_version": "2.83.0", "verifier_sha256": verifier_sha256,
                "bundle_sha256": bundle_digest, "bundle_bytes": 18267,
                "repo": "bblanchon/pdfium-binaries", "signer_workflow": workflow,
                "signer_digest": commit, "source_digest": commit,
                "source_ref": "refs/heads/master", "subject_sha256": asset_sha256,
            })
            tagged_text = (appdir / "usr/share/doc/iris/pdfium/PROVENANCE").read_text()
            self.assertIn("Attestation verifier gh version: 2.83.0", tagged_text)
            self.assertIn("Attestation verifier executable sha256: " + verifier_sha256, tagged_text)

            bad_identity = [
                ("missing-version", {"gh_version": ""}),
                ("invalid-version", {"gh_version": "gh version nope"}),
                ("leading-zero-version", {"gh_version": "gh version 02.83.0"}),
                ("oversized-version", {"gh_version": "gh version " + "1" * 65 + ".0.0"}),
                ("version-failure", {"gh_version_exit": "1"}),
                ("missing-hash", {"gh_hash": ""}),
                ("invalid-hash", {"gh_hash": "x" * 64}),
                ("hash-failure", {"gh_hash": "0" * 64, "gh_hash_exit": "1"}),
                ("unresolved-executable", {"realpath_exit": "1"}),
            ]
            for name, settings in bad_identity:
                with self.subTest(name=name):
                    rejected, appdir, gh_args = run(name, "1", token=True, **settings)
                    self.assertNotEqual(rejected.returncode, 0)
                    self.assertFalse(gh_args.exists())
                    self.assertFalse(appdir.exists())
                    self.assertFalse((root / (name + "-curl-args")).exists())

            for name, settings in (("wrong-size", {"bundle_size": "18266"}),
                                   ("wrong-bytes", {"bundle_sha256": "0" * 64}),
                                   ("fetch-failure", {"curl_exit": "22"})):
                with self.subTest(name=name):
                    rejected, appdir, gh_args = run(name, "1", token=True, **settings)
                    self.assertNotEqual(rejected.returncode, 0)
                    self.assertFalse(gh_args.exists())
                    self.assertFalse(appdir.exists())
                    curl_args = (root / (name + "-curl-args")).read_text().splitlines()
                    self.assertFalse(Path(curl_args[-2]).parent.exists())

            failed, appdir, gh_args = run("failed", "1", token=True, gh_exit="1")
            self.assertNotEqual(failed.returncode, 0)
            self.assertFalse(appdir.exists())
            self.assertFalse(Path(gh_args.read_text().splitlines()[6]).parent.exists())

            missing_token, appdir, gh_args = run("missing-token", "1")
            self.assertNotEqual(missing_token.returncode, 0)
            self.assertIn("GH_TOKEN is required", missing_token.stderr)
            self.assertFalse((root / "missing-token-curl-args").exists())
            self.assertFalse(gh_args.exists())
            self.assertFalse(appdir.exists())

            workflow = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
            manual_step = workflow.split(
                "      - name: Package Linux AppDir for manual audit", 1
            )[1].split("      - name: Package Linux AppDir for tagged release", 1)[0]
            tagged_step = workflow.split(
                "      - name: Package Linux AppDir for tagged release", 1
            )[1].split("      - name: Build Linux thumbnail smokes", 1)[0]
            self.assertIn("if: github.event_name == 'workflow_dispatch'", manual_step)
            self.assertIn('IRIS_VERIFY_PDFIUM_PROVENANCE: "0"', manual_step)
            self.assertNotIn("GH_TOKEN:", manual_step)
            self.assertIn("if: github.event_name == 'push'", tagged_step)
            self.assertIn('IRIS_VERIFY_PDFIUM_PROVENANCE: "1"', tagged_step)
            self.assertIn("GH_TOKEN:", tagged_step)
            self.assertIn('      - "v*"', workflow)

    def test_pdfium_retry_policy_is_local_to_every_package_invocation(self):
        for workflow, calls in (("ci.yml", 1), ("linux-ffmpeg-validation.yml", 1),
                                ("release.yml", 2)):
            with self.subTest(workflow=workflow):
                contents = (ROOT / ".github/workflows" / workflow).read_text()
                steps = contents.split("      - ")
                packages = [step for step in steps if "run: ./scripts/package-linux-appdir.sh" in step]
                self.assertEqual(len(packages), calls)
                for name, default in (("IRIS_PDFIUM_FETCH_RETRIES", 3),
                                      ("IRIS_PDFIUM_FETCH_RETRY_DELAY_SECONDS", 2)):
                    setting = name + ": ${{ vars." + name + " || " + str(default) + " }}"
                    self.assertEqual(contents.count(name + ":"), calls)
                    for step in packages:
                        self.assertIn("        env:\n", step)
                        self.assertIn("          " + setting, step)

    def test_verified_prefix_is_outside_cargo_cache(self):
        for script, root in (("build-linux-ffmpeg.sh", "$root"),
                             ("package-linux-appdir.sh", "$repo_root")):
            with self.subTest(script=script):
                contents = (ROOT / "scripts" / script).read_text()
                self.assertIn("${IRIS_FFMPEG_PREFIX:-" + root + "/build/linux-ffmpeg}", contents)
                self.assertNotIn("target/linux-ffmpeg", contents)
        for workflow in ("linux-ffmpeg-validation.yml", "release.yml"):
            with self.subTest(workflow=workflow):
                contents = (ROOT / ".github/workflows" / workflow).read_text()
                paths = {"FFMPEG_DIR": "", "PKG_CONFIG_LIBDIR": "/lib/pkgconfig",
                         "PKG_CONFIG_PATH": "/lib/pkgconfig"}
                if workflow == "linux-ffmpeg-validation.yml":
                    paths["IRIS_FFMPEG_PREFIX"] = ""
                for variable, suffix in paths.items():
                    self.assertIn(variable + ": ${{ github.workspace }}/build/linux-ffmpeg" + suffix,
                                  contents)
                self.assertNotIn("target/linux-ffmpeg", contents)
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        release = (ROOT / ".github/workflows/release.yml").read_text()
        release_linux = release.split("  package-linux:", 1)[1].split("  package-windows:", 1)[0]
        for workflow in (ci, release_linux):
            self.assertIn("uses: Swatinem/rust-cache@v2", workflow)
        self.assertIn("/build/", (ROOT / ".gitignore").read_text().splitlines())

    def test_ci_reuses_only_the_validated_ffmpeg_prefix(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        step = ci.split("      - name: Restore validated Linux FFmpeg runtime\n", 1)[1]
        step = step.split("      - name: Install system dependencies (Ubuntu)", 1)[0]
        self.assertIn("        if: matrix.os == 'ubuntu-latest'\n", step)
        self.assertIn("        uses: actions/cache@v4\n", step)
        self.assertIn("          path: build/linux-ffmpeg\n", step)
        self.assertIn("packaging/linux/ffmpeg.json", step)
        self.assertIn("scripts/build-linux-ffmpeg.sh", step)
        self.assertIn("scripts/linux-ffmpeg.py", step)
        self.assertIn("packaging/linux/ffmpeg-README.md", step)
        build = (ROOT / "scripts" / "build-linux-ffmpeg.sh").read_text()
        self.assertIn('if python3 "$root/scripts/linux-ffmpeg.py" validate "$prefix"; then\n        exit\n    fi', build)
        self.assertIn("cached FFmpeg prefix failed validation; rebuilding from the pinned source", build)
        self.assertIn('if [[ $prefix != "$root/build/linux-ffmpeg" ]]; then', build)
        # The cached prefix must stay outside the Cargo target tree and keep its
        # own revalidation; no gate is replaced by a bare cache hit.
        self.assertNotIn("target/linux-ffmpeg", step)

    def test_invalid_cached_prefix_is_rebuilt_only_at_default_path(self):
        # A runner image/apt change alters the system identity rechecked by
        # validate(); the cache-managed default path must self-heal by rebuilding
        # from signed source, while an explicitly chosen prefix is never deleted.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scripts = root / "scripts"
            scripts.mkdir()
            shutil.copy2(ROOT / "scripts" / "build-linux-ffmpeg.sh", scripts / "build-linux-ffmpeg.sh")
            (scripts / "linux-ffmpeg.py").write_text("import sys\nsys.exit(1)\n")
            tools = root / "bin"
            tools.mkdir()
            python = tools / "python3"
            python.write_text(f'#!/bin/sh\nexec {shutil.which("python3")} "$@"\n')
            python.chmod(0o755)
            # Keep only the helpers needed by path resolution and recovery;
            # curl is absent so the rebuilt path stops before any network.
            for helper in ("dirname", "rm"):
                (tools / helper).symlink_to(shutil.which(helper))
            env = {"PATH": str(tools), "HOME": str(root)}
            bash = shutil.which("bash")
            for name, expected in (
                (root / "build" / "linux-ffmpeg", True),
                (root / "custom-ffmpeg", False),
            ):
                with self.subTest(prefix=str(name)):
                    name.mkdir(parents=True)
                    (name / "sentinel").write_text("stale")
                    result = subprocess.run(
                        [bash, str(scripts / "build-linux-ffmpeg.sh")],
                        env=dict(env, IRIS_FFMPEG_PREFIX=str(name)),
                        capture_output=True, text=True, timeout=10)
                    # No curl in PATH stops before any network after the reuse branch.
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(name.exists(), not expected)
                    if expected:
                        self.assertIn("rebuilding from the pinned source", result.stderr)
                    else:
                        self.assertIn("refusing to delete it", result.stderr)
                        self.assertEqual((name / "sentinel").read_text(), "stale")

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
        for setting, default in (("MAX_REPORT_BYTES", 2097152),
                                 ("MAX_COMMAND_BYTES", 32768),
                                 ("MAX_METADATA_BYTES", 8388608),
                                 ("MAX_CARGO_PACKAGES", 1024),
                                 ("MAX_NATIVE_FILES", 256),
                                 ("COMMAND_SECONDS", 30)):
            self.assertIn(f"vars.IRIS_RELEASE_AUDIT_{setting} || {default}", workflow)
        native_generate = ("python3 scripts/posix-native-evidence.py generate --platform linux "
                           "--stage dist/linux --mapping packaging/native-evidence/linux-mapping.json "
                           "--output audit/native-evidence/linux.json "
                           "--report audit/native-evidence/linux-review.json")
        native_audit = ("python3 scripts/release-audit.py linux --output audit/linux-native-closure.json "
                        "--native-evidence audit/native-evidence/linux.json --require-native-evidence")
        self.assertEqual(workflow.count(native_generate), 1)
        self.assertEqual(workflow.count(native_audit), 1)
        self.assertLess(workflow.index(native_generate), workflow.index(native_audit))
        self.assertNotIn("path: audit/linux-ffmpeg.json", workflow)
        self.assertIn('IRIS_FFMPEG_MAX_REPORT_BYTES", "266240"', (ROOT / "scripts/linux-ffmpeg.py").read_text())
        self.assertEqual(workflow.count("uses: actions/upload-artifact@"), 1)
        upload = workflow.split("uses: actions/upload-artifact@", 1)[1]
        self.assertIn("name: linux-runtime-audit", upload)
        self.assertEqual(upload.split("path:", 1)[1].strip(), """|
            audit/*.json
            audit/native-evidence/linux.json
            audit/native-evidence/linux-review.json""")
        self.assertIn("--locked --offline", workflow)
        self.assertIn("staged == installed", workflow)
        self.assertIn('IRIS_VERIFY_PDFIUM_PROVENANCE: "0"', workflow)
        for setting in ('CORPUS_MAX_BYTES', 'CORPUS_TIMEOUT_SECONDS', 'CORPUS_RETRIES',
                        'CORPUS_RETRY_DELAY_SECONDS', 'LATENCY_WARMUPS', 'LATENCY_SAMPLES'):
            self.assertIn('vars.IRIS_PREVIEW_' + setting, workflow)
        self.assertIn('--example preview_corpus_smoke --locked --offline', workflow)
        self.assertIn("patchelf --set-rpath '$ORIGIN/../lib/iris' dist/linux/Iris.AppDir/usr/bin/iris-corpus-smoke", workflow)
        self.assertIn("tempfile.TemporaryDirectory(prefix='iris-corpus-'", workflow)
        self.assertIn("'scripts/fetch-preview-corpus.py', '--output', str(corpus)", workflow)
        self.assertIn("scripts/preview_corpus_report.py", workflow)
        self.assertIn("preview_report.parse_preview(", workflow)
        self.assertIn("target_os='linux'", workflow)
        self.assertNotIn("scripts/macos-ffmpeg.py", workflow)
        self.assertIn("len(report.encode()) <= int(os.environ['IRIS_FFMPEG_MAX_REPORT_BYTES'])", workflow)
        self.assertIn("pdfium['runtime']['sha256'] == pdfium_digest", workflow)
        self.assertIn("pdfium['attestation']['status'] == 'not_attempted'", workflow)
        self.assertIn("attestation_status=pdfium['attestation']['status']", workflow)
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
        # The release budget bounds the compiled product binary, the same
        # target/release/Iris artifact the workflow size gate measures. It must
        # not measure the AppDir copy after patchelf rewrites its ELF, and a
        # budget failure must be reported rather than a silent `set -e` exit.
        self.assertIn('product_binary="$target_dir/release/$binary_name"', package)
        self.assertIn('actual=$(stat -c %s "$product_binary")', package)
        self.assertIn('echo "Iris packaged binary: $actual bytes (limit: $max_bytes)"', package)
        self.assertIn('Iris packaged binary exceeds the $max_bytes-byte release budget', package)
        self.assertLess(package.index('product_binary="$target_dir/release/$binary_name"'),
                        package.index('fetch-pdfium-runtime.sh'))
        self.assertNotIn('stat -c %s "$appdir/usr/bin/$binary_name"', package)
        self.assertIn('dpkg-shlibdeps -O -xiris', package)
        self.assertNotIn('--ignore-missing-info', package.split('cat > "$deb_build_dir/debian/control"')[1])
        release_linux = (ROOT / ".github/workflows/release.yml").read_text().split('  package-linux:', 1)[1].split('  package-windows:', 1)[0]
        self.assertNotIn('libavcodec-dev', release_linux)
        self.assertNotIn('clang ffmpeg', release_linux)


    def test_release_installed_video_smoke_uses_exact_deb(self):
        release_linux = (ROOT / ".github/workflows/release.yml").read_text().split(
            "  package-linux:", 1)[1].split("  package-windows:", 1)[0]
        build = release_linux.split(
            "      - name: Build Linux thumbnail smokes", 1)[1].split(
            "      - name: Archive Linux AppDir", 1)[0]
        self.assertIn("--example pdf_thumbnail_smoke --example video_thumbnail_smoke", build)
        self.assertIn("CARGO_TARGET_DIR: ${{ github.workspace }}/target/package-linux", build)
        deb = release_linux.split(
            "      - name: Verify Debian installer", 1)[1].split(
            "      - name: Generate Linux checksums", 1)[0]
        self.assertIn("IRIS_FFMPEG_PREFIX: ${{ github.workspace }}/build/linux-ffmpeg", deb)
        self.assertIn('LD_LIBRARY_PATH="$IRIS_FFMPEG_PREFIX/lib', deb)
        self.assertIn('-f lavfi -i testsrc2=size=64x48:rate=4 -t 1 -c:v mpeg4 -an "$RUNNER_TEMP/iris-video.avi"', deb)
        invocation = deb.split("./scripts/smoke-linux-deb.sh", 1)[1]
        self.assertIn("target/package-linux/release/examples/pdf_thumbnail_smoke", invocation)
        self.assertIn('target/package-linux/release/examples/video_thumbnail_smoke "$RUNNER_TEMP/iris-video.avi"', invocation)
        # Neither the probe nor the clip is staged into the release package.
        self.assertNotIn("dist/linux/Iris.AppDir/usr/bin/iris-video-smoke", release_linux)
        self.assertNotIn("dist/linux/iris-video.avi", release_linux)


if __name__ == "__main__":
    unittest.main()
