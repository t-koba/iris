#!/usr/bin/env python3
"""Offline contracts for the reviewed macOS/Linux native-evidence producer."""
from contextlib import contextmanager
from importlib.util import module_from_spec, spec_from_file_location
from contextlib import redirect_stderr
import hashlib
import io
import json
import shutil
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
from unittest import TestCase, main
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))


def load(name, path):
    spec = spec_from_file_location(name, path)
    module = module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


producer = load("posix_native_evidence", ROOT / "scripts/posix-native-evidence.py")
audit_module = load("release_audit_contract", ROOT / "scripts/release-audit.py")
policy_module = load("release_policy_contract", ROOT / "scripts/release-policy.py")
from release_evidence import validate_closure  # noqa: E402

STATIC_SYMBOLS = ("_lzma_code", "_lzma_end", "_lzma_stream_decoder", "_lzma_stream_buffer_decode")
STATIC_DYLIB = "Iris.app/Contents/Frameworks/libavcodec.62.dylib"
FFMPEG_DOCS = "Iris.app/Contents/Resources/ffmpeg/"
MACOS_NATIVE = ("Iris.app/Contents/MacOS/Iris",
                "Iris.app/Contents/Frameworks/libavcodec.62.dylib",
                "Iris.app/Contents/Frameworks/libswresample.4.dylib")
LINUX_NATIVE = ("Iris.AppDir/usr/bin/Iris",
                "Iris.AppDir/usr/lib/iris/libavcodec.so.62",
                "Iris.AppDir/usr/lib/iris/libavformat.so.61",
                "Iris.AppDir/usr/lib/iris/libavutil.so.60",
                "Iris.AppDir/usr/lib/iris/libswresample.so.5",
                "Iris.AppDir/usr/lib/iris/libswscale.so.8",
                "Iris.AppDir/usr/lib/iris/libpdfium.so")
MACOS_EXTRA = ("Iris.app/Contents/Info.plist", "Iris.app/Contents/Resources/Iris.icns",
               "Iris.app/Contents/Resources/ffmpeg-runtime.json",
               "Iris.app/Contents/_CodeSignature/CodeResources",
               "Iris.app/Contents/Resources/ffmpeg/config.h",
               "Iris-macos.zip", "Iris-macos.zip.sha256",
               "Iris-macos.pkg", "Iris-macos.pkg.sha256")
LINUX_EXTRA = ("Iris.AppDir/AppRun", "Iris.AppDir/iris.desktop", "Iris.AppDir/iris.png",
               "Iris.AppDir/usr/share/applications/iris.desktop",
               "Iris.AppDir/usr/share/icons/hicolor/256x256/apps/iris.png",
               "Iris.AppDir/usr/share/doc/iris/pdfium/args.gn",
               "Iris.AppDir/usr/share/doc/iris/pdfium/licenses/pdfium.txt",
               "Iris.AppDir/usr/share/doc/iris/ffmpeg/config.h",
               "ffmpeg-audit.json", "Iris-linux-appdir.tar.gz",
               "Iris-linux-appdir.tar.gz.sha256", "iris_1.0_amd64.deb",
               "iris_1.0_amd64.deb.sha256")


def mapping(platform):
    return json.loads((ROOT / "packaging/native-evidence" /
                       (platform + "-mapping.json")).read_text(encoding="utf-8"))


def write_file(root, name, native=False):
    path = root / name
    path.parent.mkdir(parents=True, exist_ok=True)
    if native:
        magic = b"\x7fELF" if name.startswith("Iris.AppDir") else b"\xfe\xed\xfa\xcf"
        path.write_bytes(magic + b"synthetic native bytes, NOT a release binary")
    else:
        path.write_text("synthetic review evidence, NOT legal approval\n", encoding="utf-8")
    return path


def lzma_pin(stage):
    """Synthetic pin whose hashes bind to this offline stage's evidence bytes."""
    pin = json.loads((ROOT / "packaging/macos/liblzma.json").read_text(encoding="utf-8"))
    pin["sha256"] = producer.sha256(stage / (FFMPEG_DOCS + "xz-source.tar.xz"))
    pin["license"] = dict(pin["license"],
                          sha256=producer.sha256(stage / (FFMPEG_DOCS + "xz-COPYING.0BSD")))
    return pin


def write_macos_static_evidence(stage):
    pin = lzma_pin(stage)
    docs_dir = stage / FFMPEG_DOCS
    digests_by_name = {path.name: producer.sha256(path) for path in docs_dir.iterdir()}
    evidence = dict(source=pin,
                    signature=dict(verified=True, primary_fingerprint=pin["fingerprint"],
                                   signing_fingerprint=pin["fingerprint"]),
                    license_metadata_sha256=digests_by_name["xz-COPYING.0BSD"],
                    static_archive_sha256="ab" * 32, cmake="cmake version test synthetic")
    library = producer.sha256(stage / STATIC_DYLIB)
    runtime = dict(liblzma=evidence,
                   static_libraries={Path(STATIC_DYLIB).name: dict(
                       sha256=library, symbols=list(STATIC_SYMBOLS))})
    (stage / producer.RUNTIME_REPORT).write_text(json.dumps(runtime, indent=2) + "\n",
                                                 encoding="utf-8")
    build_manifest = dict(liblzma=evidence, documents=digests_by_name,
                          staged_libraries={Path(STATIC_DYLIB).name: library})
    (stage / producer.BUILD_MANIFEST).write_text(json.dumps(build_manifest, indent=2) + "\n",
                                                 encoding="utf-8")


def make_stage(root, platform):
    stage = root / ("dist/" + platform)
    stage.mkdir(parents=True)
    value = mapping(platform)
    for name in value["evidence_files"]:
        write_file(stage, name)
    natives = MACOS_NATIVE if platform == "macos" else LINUX_NATIVE
    extra = MACOS_EXTRA if platform == "macos" else LINUX_EXTRA
    for name in (*natives, *extra):
        write_file(stage, name, native=name in natives)
    if platform == "linux":
        (stage / "Iris.AppDir/usr/lib/iris/libavcodec.so").symlink_to("libavcodec.so.62")
    else:
        write_macos_static_evidence(stage)
    return stage


def write_mapping(root, platform, value):
    path = root / (platform + "-mapping.json")
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path


def generate(platform, stage, output, mapping_path=None, report=None):
    argv = ["generate", "--platform", platform, "--stage", str(stage),
            "--mapping", str(mapping_path or ROOT / "packaging/native-evidence" /
                             (platform + "-mapping.json")),
            "--output", str(output)]
    if report:
        argv.extend(["--report", str(report)])
    return producer.main(argv)


def digests(stage):
    return {path.relative_to(stage).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in stage.rglob("*") if path.is_file()}


def collect(stage, sidecar, manifest=None, max_components=16):
    args = SimpleNamespace(max_native_files=256, max_native_components=max_components,
                           max_report_bytes=2 * 1024 * 1024, max_command_bytes=32768,
                           max_metadata_bytes=8388608, max_cargo_packages=1024,
                           max_command_seconds=30, command_seconds=30, platform="",
                           output=None, native_evidence=sidecar,
                           require_native_evidence=True)
    report = audit_module.Audit(args)
    if manifest:
        # Mirrors the native-file records release-audit.py collects from its
        # otool/ldd closure before it validates the native evidence sidecar.
        for row in manifest["components"]:
            artifact = row["artifact"]
            path = stage / artifact["path"] if not artifact["path"].startswith("/") else Path(artifact["path"])
            if not path.is_file():
                continue
            data = dict(path=str(path.absolute()), sha256=artifact["sha256"],
                        bytes=path.stat().st_size)
            report.add("native-file", **data)
            report.add("file", **data)
    report.native_evidence(stage, sidecar)
    return report


@contextmanager
def offline_scan(stage, symbols=STATIC_SYMBOLS, native_names=None):
    names = {Path(STATIC_DYLIB).name} if native_names is None else set(native_names)

    def tool(argv, seconds=30):
        if list(argv[:2]) != ["nm", "-gU"]:
            raise AssertionError("unexpected native tool call: " + str(argv))
        if Path(argv[-1]).name not in names:
            return ""
        return "".join(f"0000000000001234 T {symbol}\n" for symbol in symbols)

    original_pin = producer.lzma_pin

    def local_pin(mapping):
        return lzma_pin(stage) if stage.name == "macos" else original_pin(mapping)

    with patch.object(producer, "external_dependencies", return_value={}), \
            patch.object(producer, "run_tool", side_effect=tool), \
            patch.object(producer, "lzma_pin", side_effect=local_pin):
        yield


class PosixNativeEvidence(TestCase):
    def check_stage(self, root, platform):
        stage = make_stage(root, platform)
        sidecar = root / ("generated/" + platform + ".json")
        with offline_scan(stage):
            self.assertEqual(generate(platform, stage, sidecar,
                                      report=root / ("generated/" + platform + "-review.json")), 0,
                             "producer must accept a fully mapped synthetic stage")
        manifest = json.loads(sidecar.read_text(encoding="utf-8"))
        self.assertEqual(set(manifest), {"schema", "artifacts", "components"})
        self.assertEqual(manifest["schema"], 1)
        report = collect(stage, sidecar, manifest)
        self.assertFalse(any(row["kind"] == "missing" for row in report.records), report.records)
        closure = [row for row in report.records if row["kind"] == "native-closure"]
        self.assertEqual(len(closure), 1)
        components = validate_closure(closure[0]["evidence"], report.records, 256, 16)
        covered = {row["artifact"]["path"] for row in components}
        native_paths = {row["path"] for row in report.records if row["kind"] == "native-file"}
        self.assertTrue(native_paths <= covered)
        self.assertEqual(len({row["id"] for row in components}), len(components))
        for row in components:
            if ("avcodec" in row["artifact"]["path"] or "swresample" in row["artifact"]["path"]) \
                    and row["mode"] == "dynamic":
                self.assertEqual(row["license"]["id"], "LGPL-2.1-or-later")
                self.assertEqual(row["license"]["disposition"], "review")
                for key in ("notice", "source", "relinking"):
                    self.assertEqual(row["obligations"][key]["status"], "provided")
        static = [row for row in components if row["mode"] == "static"]
        if platform == "macos":
            self.assertEqual(len(static), 1)
            self.assertEqual(Path(static[0]["artifact"]["path"]), stage / STATIC_DYLIB)
            self.assertEqual(static[0]["license"]["id"], "0BSD")
            self.assertEqual(static[0]["license"]["disposition"], "allowed")
            self.assertEqual(static[0]["source"]["revision"],
                             json.loads((ROOT / "packaging/macos/liblzma.json").read_text())["sha256"])
            for key in ("notice", "source", "relinking"):
                self.assertEqual(static[0]["obligations"][key]["status"], "provided")
        else:
            self.assertFalse(static)
        review = json.loads((root / ("generated/" + platform + "-review.json")).read_text())
        self.assertEqual(review["platform"], platform)
        self.assertTrue(review["mapping"]["sha256"])
        return stage, sidecar, manifest

    def test_macos_stage_produces_a_passing_schema2_closure(self):
        with tempfile.TemporaryDirectory() as directory:
            self.check_stage(Path(directory), "macos")

    def test_macos_truncated_static_report_cannot_hide_coverage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage, _, _ = self.check_stage(root, "macos")
            other = "Iris.app/Contents/Frameworks/libswresample.4.dylib"
            names = {Path(STATIC_DYLIB).name, Path(other).name}
            with offline_scan(stage, native_names=names):
                self.assertEqual(generate("macos", stage, root / "truncated.json"), 0)
            manifest = json.loads((root / "truncated.json").read_text(encoding="utf-8"))
            covered = {row["artifact"]["path"] for row in manifest["components"]
                       if row["mode"] == "static"}
            self.assertEqual(covered, {STATIC_DYLIB, other})

    def test_macos_static_liblzma_evidence_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage, _, _ = self.check_stage(root, "macos")
            runtime_path, build_path = stage / producer.RUNTIME_REPORT, stage / producer.BUILD_MANIFEST
            baseline_runtime = json.loads(runtime_path.read_text(encoding="utf-8"))
            baseline_build = json.loads(build_path.read_text(encoding="utf-8"))
            basename = Path(STATIC_DYLIB).name

            def refuse(mutate, expected, symbols=STATIC_SYMBOLS):
                runtime_path.write_text(json.dumps(baseline_runtime), encoding="utf-8")
                build_path.write_text(json.dumps(baseline_build), encoding="utf-8")
                mutate()
                output = root / "rejected.json"
                errors = io.StringIO()
                with offline_scan(stage, symbols=symbols), redirect_stderr(errors):
                    self.assertNotEqual(generate("macos", stage, output), 0)
                self.assertIn(expected, errors.getvalue())
                self.assertFalse(output.exists())

            def edit_static_evidence(field, value):
                def mutate():
                    for path in (runtime_path, build_path):
                        evidence = json.loads(path.read_text(encoding="utf-8"))
                        evidence["liblzma"][field] = value
                        path.write_text(json.dumps(evidence), encoding="utf-8")
                return mutate

            refuse(lambda: runtime_path.write_text(json.dumps(dict(
                liblzma=baseline_runtime["liblzma"], static_libraries={})), encoding="utf-8"),
                "runtime lacks verified static library evidence")
            refuse(lambda: runtime_path.write_text(json.dumps(dict(
                baseline_runtime, static_libraries={basename: dict(sha256="cd" * 32,
                symbols=list(STATIC_SYMBOLS))})), encoding="utf-8"),
                "static liblzma bytes differ from the staged dylib")
            refuse(edit_static_evidence("source", dict(baseline_runtime["liblzma"]["source"], version="0.0.0")),
                   "runtime liblzma evidence does not match the reviewed pin")
            refuse(lambda: build_path.write_text(json.dumps(dict(
                baseline_build, liblzma=dict(baseline_build["liblzma"], sha256="ee" * 32))),
                encoding="utf-8"), "runtime and build liblzma evidence differ")
            refuse(lambda: None, "static liblzma symbols absent from the staged dylib",
                   symbols=("_lzma_code",))
            refuse(lambda: (stage / (FFMPEG_DOCS + "xz-source.tar.xz")).unlink(),
                   "reviewed supporting evidence absent from stage")
            runtime_path.write_text(json.dumps(baseline_runtime), encoding="utf-8")
            build_path.write_text(json.dumps(baseline_build), encoding="utf-8")
            write_file(stage, FFMPEG_DOCS + "xz-source.tar.xz")
            with offline_scan(stage):
                self.assertEqual(generate("macos", stage, root / "restored.json"), 0)

    def test_linux_stage_produces_a_passing_schema2_closure(self):
        with tempfile.TemporaryDirectory() as directory:
            self.check_stage(Path(directory), "linux")

    def test_linux_pdfium_pin_matches_the_authoritative_release_pin(self):
        pin = json.loads((ROOT / "packaging/linux/pdfium.json").read_text(encoding="utf-8"))
        expected_bytes, expected_sha = audit_module.PDFIUM_ARCHIVES[pin["asset"]]
        self.assertEqual(pin["bytes"], expected_bytes)
        self.assertEqual(pin["sha256"], expected_sha)
        self.assertEqual(pin["attestation_sha256"], audit_module.PDFIUM_ATTESTATION["pin_sha256"])
        self.assertEqual(pin["signer_digest"], audit_module.PDFIUM_COMMIT)
        self.assertEqual(pin["source_digest"], audit_module.PDFIUM_COMMIT)
        fetch = (ROOT / "scripts/fetch-pdfium-runtime.sh").read_text(encoding="utf-8")
        self.assertIn(f'asset="{pin["asset"]}"', fetch)
        self.assertIn(f'asset_bytes={expected_bytes}', fetch)
        self.assertIn(f'asset_sha256="{expected_sha}"', fetch)
        self.assertIn(f'pdfium_release="{pin["release"]}"', fetch)
        self.assertEqual(pin["url"],
                         "https://github.com/" + audit_module.PDFIUM_ATTESTATION["repo"] +
                         "/releases/download/" + pin["release"] + "/" + pin["asset"])

    def test_repository_relative_mapping_path_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = make_stage(root, "linux")
            output = root / "generated/relative.json"
            report = root / "generated/relative-review.json"
            with offline_scan(stage):
                self.assertEqual(generate("linux", stage, output,
                                          mapping_path=Path("packaging/native-evidence/linux-mapping.json"),
                                          report=report), 0)
            review = json.loads(report.read_text(encoding="utf-8"))
            self.assertEqual(review["mapping"]["path"],
                             "packaging/native-evidence/linux-mapping.json")
            manifest = json.loads(output.read_text(encoding="utf-8"))
            pdfium = [row for row in manifest["components"]
                      if row["name"] == "bundled PDFium shared library"]
            self.assertEqual(len(pdfium), 1)
            self.assertEqual(pdfium[0]["source"]["origin"],
                             "https://github.com/bblanchon/pdfium-binaries/releases/download/"
                             "chromium/8138/pdfium-linux-x64.tgz")
            self.assertEqual(pdfium[0]["source"]["revision"],
                             "d6b1e28950a0392590d79bbdbb7eb19686872b688f8a36f170acb19e41473113")

    def test_missing_and_tampered_evidence_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage, sidecar, _ = self.check_stage(root, "linux")
            evidence = mapping("linux")["evidence_files"][0]
            (stage / evidence).unlink()
            with offline_scan(stage):
                self.assertNotEqual(generate("linux", stage, root / "missing.json"), 0)
            write_file(stage, evidence)
            with offline_scan(stage):
                self.assertEqual(generate("linux", stage, sidecar), 0)
            before = digests(stage)
            first = sidecar.read_bytes()
            library = stage / "Iris.AppDir/usr/lib/iris/libavcodec.so.62"
            library.write_bytes(b"\x7fELF" + b"tampered after evidence generation")
            failed = collect(stage, sidecar, json.loads(sidecar.read_text()))
            self.assertTrue(any(row["kind"] == "missing" for row in failed.records))
            self.assertFalse(any(row["kind"] == "native-closure" for row in failed.records))
            with offline_scan(stage):
                self.assertEqual(generate("linux", stage, sidecar), 0)
            self.assertNotEqual(sidecar.read_bytes(), first)
            self.assertEqual(set(before), set(digests(stage)))

    def test_unmapped_and_unlicensed_native_files_fail(self):
        for platform, unlisted in (("macos", "Iris.app/Contents/MacOS/Helper"),
                                   ("linux", "Iris.AppDir/usr/bin/Helper")):
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                stage = make_stage(root, platform)
                write_file(stage, unlisted, native=True)
                with offline_scan(stage):
                    self.assertNotEqual(generate(
                        platform, stage, root / "rejected.json"), 0)
                self.assertFalse((root / "rejected.json").exists())
        review_dir = Path(tempfile.mkdtemp(prefix=".iris-native-evidence-test-", dir=ROOT))
        try:
            root = review_dir
            stage = make_stage(root, "macos")
            value = mapping("macos")
            del value["components"]["ffmpeg-{basename}"]
            errors = io.StringIO()
            with offline_scan(stage), redirect_stderr(errors):
                self.assertNotEqual(generate(
                    "macos", stage, root / "rejected.json",
                    mapping_path=write_mapping(root, "macos", value)), 0)
            self.assertIn("no reviewed component rule for staged native file", errors.getvalue())
            self.assertNotIn("reviewed mapping must be inside the repository", errors.getvalue())
        finally:
            shutil.rmtree(review_dir, ignore_errors=True)

    def test_stage_bytes_are_never_modified_and_sidecar_stays_outside(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = make_stage(root, "macos")
            before = digests(stage)
            with offline_scan(stage):
                self.assertNotEqual(generate(
                    "macos", stage, stage / "Iris-macos.zip"), 0)
                self.assertEqual(generate("macos", stage, root / "outside.json"), 0)
            self.assertEqual(before, digests(stage))
            self.assertFalse((stage / "outside.json").exists())

    def test_external_dependency_parsers_and_package_mapping(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            lib = root / "libm.so.6"
            lib.write_bytes(b"synthetic ELF")
            with patch.object(producer, "run_tool", return_value=(
                    "\tlinux-vdso.so.1 (0x00007fff)\n"
                    f"\tlibm.so.6 => {lib} (0x00007aaa)\n"
                    "\tlibmissing.so.1 => not found\n")):
                with self.assertRaisesRegex(ValueError, "unresolved ELF dependency"):
                    producer.ldd_dependencies(root / "Iris")
            with patch.object(producer, "run_tool", return_value=(
                    f"\tlibm.so.6 => {lib} (0x00007aaa)\n")):
                self.assertEqual(producer.ldd_dependencies(root / "Iris"), {lib.resolve()})
            with patch.object(producer, "run_tool", side_effect=(
                    "\tIris:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n"
                    f"\t@loader_path/{lib.name} (compatibility version 1.0.0)\n", "")):
                self.assertEqual(producer.otool_dependencies(root / "Iris"), {lib.resolve()})
            with patch.object(producer, "run_tool", return_value="libm6: /lib/libm.so.6\n"):
                self.assertEqual(producer.system_package(lib), "libm6")

    def test_generated_ffmpeg_components_satisfy_the_policy_binding(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage, sidecar, manifest = self.check_stage(root, "linux")
            closure = collect(stage, sidecar, manifest)
            policy = policy_module.load_policy(ROOT / "scripts/release-policy.json", 65536)
            records, probed = [], set()
            for path in (stage / name for name in producer.walk_stage(stage, 256)):
                name = path.name
                if producer.ffmpeg_component(name) is None:
                    continue
                resolved = str(path.absolute())
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                value = {"library": resolved}
                for component in (name, producer.ffmpeg_component(name)):
                    value[component + "_configuration"] = "--disable-gpl"
                    value[component + "_license"] = "LGPL version 2.1 or later"
                records.append(dict(kind="file", path=resolved, sha256=digest,
                                    bytes=path.stat().st_size))
                records.append(dict(kind="command", status=0, truncated=False,
                                    argv=["python3", "scripts/release-audit.py",
                                          "probe-library", resolved],
                                    text=json.dumps(value)))
                probed.add(producer.ffmpeg_component(name))
            self.assertTrue({"avcodec", "avformat", "avutil", "swscale"} <= probed)
            self.assertFalse(policy_module.native(records, policy),
                             "reviewed FFmpeg components must satisfy native() license checks")
            self.assertTrue(any(row["kind"] == "native-closure" for row in closure.records))


if __name__ == "__main__":
    main()
