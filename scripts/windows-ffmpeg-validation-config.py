#!/usr/bin/env python3
"""Fail-closed validation of the production Windows native-preview build inputs.

Composes the pinned vcpkg manifest/config/triplet, the verified source and PDFium
pins, the stock binding ABI, the overlay feature gate and the release hold into
one evidence manifest. No network, no vcpkg and no build occurs here: this is
the offline contract the Windows runner must satisfy before any native work.
"""

import argparse
import hashlib
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

import importlib.util


def load(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def require(ok, message):
    if not ok:
        raise ValueError(message)


BASELINE = "b8dfe957ec8b5c0ef5c4b31eea594b8486af744f"
TRIPLET = "x64-windows-iris-dynamic"
FORBIDDEN_FEATURES = ("gpl", "version3", "nonfree")


def read_json(relative):
    return json.loads((ROOT / relative).read_text())


def sha256_file(relative):
    return hashlib.sha256((ROOT / relative).read_bytes()).hexdigest()


def _comment_keys(mapping):
    return {key for key in mapping if key.startswith("$")}


def validate_manifest(manifest):
    # Allowlist every top-level key: any version control (version, version>=,
    # port-version, overrides, constraints, builtin-baseline besides the pin)
    # would let vcpkg resolve a different port while this gate stayed green.
    allowed_top = {"name", "version", "builtin-baseline", "dependencies"} | _comment_keys(manifest)
    require(set(manifest) <= allowed_top, "unexpected manifest top-level key")
    require(manifest["name"] == "iris-windows-native-validation", "unexpected validation manifest name")
    require(manifest["version"] == "0.0.0", "unexpected manifest version")
    require(manifest["builtin-baseline"] == BASELINE, "manifest baseline drifted")
    deps = manifest["dependencies"]
    require(len(deps) == 1 and deps[0]["name"] == "ffmpeg", "manifest must declare only ffmpeg")
    dep = deps[0]
    allowed_dep = {"name", "default-features"} | _comment_keys(dep)
    require(set(dep) <= allowed_dep, "unexpected ffmpeg dependency key (version/override)")
    require(dep.get("default-features") is True, "LGPL default features must be enabled")
    return True


def validate_configuration(config):
    # Allowlist the real vcpkg-configuration.json fields. registries,
    # overlay-ports and overlay-triplets all change which portfile/triplet
    # vcpkg builds and must not appear in the checked-in validation config.
    allowed_top = {"default-registry"} | _comment_keys(config)
    require(set(config) <= allowed_top, "unexpected configuration key (registries/overlay)")
    reg = config["default-registry"]
    allowed_reg = {"kind", "repository", "baseline"} | _comment_keys(reg)
    require(set(reg) <= allowed_reg, "unexpected default-registry key")
    require(reg["kind"] == "git", "registry kind drifted")
    require(reg["repository"] == "https://github.com/microsoft/vcpkg", "registry repository drifted")
    require(reg["baseline"] == BASELINE, "registry baseline drifted")
    return True


def validate_triplet(text):
    require("set(VCPKG_TARGET_ARCHITECTURE x64)" in text, "triplet must target x64")
    require("set(VCPKG_CRT_LINKAGE dynamic)" in text, "triplet CRT must be dynamic")
    require("set(VCPKG_LIBRARY_LINKAGE dynamic)" in text, "triplet libraries must be dynamic")
    require("FATAL_ERROR" in text and "static" in text, "triplet must fail closed on static linkage")
    return True


def validate_release_hold(release_yml, package_ps1, release_audit, release_policy, ci_yml):
    require("IRIS_BUILD_VARIANT: minimal" in release_yml,
            "release.yml must keep the minimal Windows build variant")
    require("--no-default-features" in package_ps1 and '"--features", "minimal"' in package_ps1,
            "package-windows.ps1 must keep the minimal feature build")
    require('variant="minimal"' in release_audit, "release audit must keep the Windows minimal variant")
    require(release_policy["required_variants"]["windows"] == "product",
            "release policy must still require the product Windows variant (the hold)")
    require("python3 scripts/test-windows-ffmpeg-validation-config.py" in ci_yml,
            "validation config contract must be wired into CI")
    return True


def build():
    source = load("wfs", "scripts/windows-ffmpeg-source.py")
    overlay = load("wfo", "scripts/windows-ffmpeg-overlay.py")

    manifest = read_json("packaging/windows/validation/vcpkg.json")
    configuration = read_json("packaging/windows/validation/vcpkg-configuration.json")
    triplet_path = "packaging/windows/validation/triplets/%s.cmake" % TRIPLET
    triplet = (ROOT / triplet_path).read_text()

    validate_manifest(manifest)
    validate_configuration(configuration)
    validate_triplet(triplet)

    pin = source.source_pin()
    pdfium = source.pdfium_pin()
    features = overlay.validate_features(["avcodec", "avformat", "avfilter", "avdevice",
                                          "swresample", "swscale"])
    for feature in features:
        require(feature not in FORBIDDEN_FEATURES, "forbidden feature selected")

    validate_release_hold(
        (ROOT / ".github/workflows/release.yml").read_text(),
        (ROOT / "scripts/package-windows.ps1").read_text(),
        (ROOT / "scripts/release-audit.py").read_text(),
        read_json("scripts/release-policy.json"),
        (ROOT / ".github/workflows/ci.yml").read_text(),
    )

    return dict(
        vcpkg_baseline=BASELINE,
        triplet=TRIPLET,
        manifest_sha256=sha256_file("packaging/windows/validation/vcpkg.json"),
        configuration_sha256=sha256_file("packaging/windows/validation/vcpkg-configuration.json"),
        triplet_sha256=sha256_file(triplet_path),
        source_version=pin["version"],
        source_sha256=pin["sha256"],
        source_url=pin["url"],
        source_fingerprint=pin["fingerprint"],
        avcodec_version=pin["avcodec_version"],
        pdfium_release=pdfium["release"],
        pdfium_sha256=pdfium["sha256"],
        lgpl_features=features,
        forbidden_features=list(FORBIDDEN_FEATURES),
        bindings={"ffmpeg-next": "9.0.0", "ffmpeg-sys-next": "9.0.1"},
        release_hold="minimal until exact Windows-runner evidence",
        publishes_release_artifacts=False,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", nargs="?", type=Path)
    args = parser.parse_args()
    evidence = build()
    text = json.dumps(evidence, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text)
    else:
        print(text, end="")


if __name__ == "__main__":
    main()
