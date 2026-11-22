#!/usr/bin/env python3
"""Offline contracts for the reviewed macOS/Linux native-evidence producer."""
from contextlib import contextmanager
from importlib.util import module_from_spec, spec_from_file_location
import hashlib
import json
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
def offline_scan():
    with patch.object(producer, "external_dependencies", return_value={}):
        yield


class PosixNativeEvidence(TestCase):
    def check_stage(self, root, platform):
        stage = make_stage(root, platform)
        sidecar = root / ("generated/" + platform + ".json")
        with offline_scan():
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
            if "avcodec" in row["artifact"]["path"] or "swresample" in row["artifact"]["path"]:
                self.assertEqual(row["license"]["id"], "LGPL-2.1-or-later")
                self.assertEqual(row["license"]["disposition"], "review")
                self.assertEqual(row["mode"], "dynamic")
                for key in ("notice", "source", "relinking"):
                    self.assertEqual(row["obligations"][key]["status"], "provided")
        review = json.loads((root / ("generated/" + platform + "-review.json")).read_text())
        self.assertEqual(review["platform"], platform)
        self.assertTrue(review["mapping"]["sha256"])
        return stage, sidecar, manifest

    def test_macos_stage_produces_a_passing_schema2_closure(self):
        with tempfile.TemporaryDirectory() as directory:
            self.check_stage(Path(directory), "macos")

    def test_linux_stage_produces_a_passing_schema2_closure(self):
        with tempfile.TemporaryDirectory() as directory:
            self.check_stage(Path(directory), "linux")

    def test_repository_relative_mapping_path_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = make_stage(root, "linux")
            output = root / "generated/relative.json"
            report = root / "generated/relative-review.json"
            with offline_scan():
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
            with offline_scan():
                self.assertNotEqual(generate("linux", stage, root / "missing.json"), 0)
            write_file(stage, evidence)
            with offline_scan():
                self.assertEqual(generate("linux", stage, sidecar), 0)
            before = digests(stage)
            first = sidecar.read_bytes()
            library = stage / "Iris.AppDir/usr/lib/iris/libavcodec.so.62"
            library.write_bytes(b"\x7fELF" + b"tampered after evidence generation")
            failed = collect(stage, sidecar, json.loads(sidecar.read_text()))
            self.assertTrue(any(row["kind"] == "missing" for row in failed.records))
            self.assertFalse(any(row["kind"] == "native-closure" for row in failed.records))
            with offline_scan():
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
                with offline_scan():
                    self.assertNotEqual(generate(
                        platform, stage, root / "rejected.json"), 0)
                self.assertFalse((root / "rejected.json").exists())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = make_stage(root, "macos")
            value = mapping("macos")
            del value["components"]["ffmpeg-{basename}"]
            with offline_scan():
                self.assertNotEqual(generate(
                    "macos", stage, root / "rejected.json",
                    mapping_path=write_mapping(root, "macos", value)), 0)

    def test_stage_bytes_are_never_modified_and_sidecar_stays_outside(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = make_stage(root, "macos")
            before = digests(stage)
            with offline_scan():
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
