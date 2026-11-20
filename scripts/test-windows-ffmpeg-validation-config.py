#!/usr/bin/env python3
"""Offline contracts for the production Windows validation build-input gate."""

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("vc", ROOT / "scripts/windows-ffmpeg-validation-config.py")
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)


class ManifestContracts(unittest.TestCase):
    def test_manifest_is_lgpl_only_and_pinned(self):
        m = mod.read_json("packaging/windows/validation/vcpkg.json")
        self.assertTrue(mod.validate_manifest(m))
        self.assertEqual(m["builtin-baseline"], mod.BASELINE)

    def test_manifest_rejects_extra_features(self):
        m = mod.read_json("packaging/windows/validation/vcpkg.json")
        m["dependencies"][0]["features"] = ["gpl"]
        with self.assertRaises(ValueError):
            mod.validate_manifest(m)

    def test_manifest_rejects_version_constraints(self):
        base = mod.read_json("packaging/windows/validation/vcpkg.json")
        for key in ("version", "version>=", "version=", "port-version", "override"):
            with self.subTest(key=key):
                m = copy.deepcopy(base)
                m["dependencies"][0][key] = "9.1.0"
                with self.assertRaises(ValueError):
                    mod.validate_manifest(m)

    def test_manifest_rejects_top_level_overrides_and_constraints(self):
        base = mod.read_json("packaging/windows/validation/vcpkg.json")
        for key, value in (("overrides", [{"name": "ffmpeg", "version": "9.1.0"}]),
                           ("constraints", {"ffmpeg": "9.1.0"}),
                           ("default-registry", {})):
            with self.subTest(key=key):
                m = copy.deepcopy(base)
                m[key] = value
                with self.assertRaises(ValueError):
                    mod.validate_manifest(m)

    def test_manifest_rejects_baseline_drift(self):
        m = mod.read_json("packaging/windows/validation/vcpkg.json")
        m["builtin-baseline"] = "deadbeef"
        with self.assertRaises(ValueError):
            mod.validate_manifest(m)

    def test_manifest_rejects_disabled_default_features(self):
        m = mod.read_json("packaging/windows/validation/vcpkg.json")
        m["dependencies"][0]["default-features"] = False
        with self.assertRaises(ValueError):
            mod.validate_manifest(m)


class ConfigurationContracts(unittest.TestCase):
    def test_registry_pinned(self):
        c = mod.read_json("packaging/windows/validation/vcpkg-configuration.json")
        self.assertTrue(mod.validate_configuration(c))

    def test_registry_rejects_real_vcpkg_provenance_fields(self):
        base = mod.read_json("packaging/windows/validation/vcpkg-configuration.json")
        injections = (
            ("registries", [{"kind": "git", "repository": "https://example.invalid/ports",
                             "reference": "unsigned", "packages": ["ffmpeg"]}]),
            ("overlay-ports", ["../prototype/windows-ffmpeg/overlay-ports"]),
            ("overlay-triplets", ["../prototype/windows-ffmpeg/triplets"]),
            ("overrides", []),
            ("overlay-registries", []),
            ("default-registry-extra", None),
        )
        for key, value in injections:
            if key == "default-registry-extra":
                c = copy.deepcopy(base)
                c["default-registry"]["tools"] = "git"
            else:
                c = copy.deepcopy(base)
                c[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                mod.validate_configuration(c)


class TripletContracts(unittest.TestCase):
    def test_triplet_is_dynamic_x64_fail_closed(self):
        text = (ROOT / "packaging/windows/validation/triplets/x64-windows-iris-dynamic.cmake").read_text()
        self.assertTrue(mod.validate_triplet(text))

    def test_triplet_rejects_static_crt(self):
        # Isolate the CRT guard: every other check passes.
        text = ('set(VCPKG_TARGET_ARCHITECTURE x64)\n'
                'set(VCPKG_CRT_LINKAGE static)\n'
                'set(VCPKG_LIBRARY_LINKAGE dynamic)\n'
                'if(static) message(FATAL_ERROR "static")\n')
        with self.assertRaises(ValueError):
            mod.validate_triplet(text)

    def test_triplet_rejects_static_library(self):
        text = ('set(VCPKG_TARGET_ARCHITECTURE x64)\n'
                'set(VCPKG_CRT_LINKAGE dynamic)\n'
                'set(VCPKG_LIBRARY_LINKAGE static)\n'
                'if(static) message(FATAL_ERROR "static")\n')
        with self.assertRaises(ValueError):
            mod.validate_triplet(text)

    def test_triplet_rejects_no_static_fail_closed(self):
        text = ('set(VCPKG_TARGET_ARCHITECTURE x64)\n'
                'set(VCPKG_CRT_LINKAGE dynamic)\n'
                'set(VCPKG_LIBRARY_LINKAGE dynamic)\n')
        with self.assertRaises(ValueError):
            mod.validate_triplet(text)


class CompositionContracts(unittest.TestCase):
    def test_build_composes_all_pins(self):
        e = mod.build()
        self.assertEqual(e["vcpkg_baseline"], mod.BASELINE)
        self.assertEqual(e["source_version"], "8.1.3")
        self.assertEqual(e["source_sha256"],
                         "7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3")
        self.assertEqual(e["avcodec_version"], [62, 28, 103])
        self.assertEqual(e["pdfium_release"], "chromium/8138")
        self.assertEqual(e["lgpl_features"],
                         ["avcodec", "avformat", "avfilter", "avdevice", "swresample", "swscale"])
        self.assertEqual(e["bindings"], {"ffmpeg-next": "9.0.0", "ffmpeg-sys-next": "9.0.1"})
        self.assertFalse(e["publishes_release_artifacts"])

    def test_build_records_input_hashes(self):
        e = mod.build()
        for key in ("manifest_sha256", "configuration_sha256", "triplet_sha256"):
            self.assertEqual(len(e[key]), 64)
            self.assertTrue(all(c in "0123456789abcdef" for c in e[key]))

    def test_build_declares_forbidden_features_and_hold(self):
        e = mod.build()
        self.assertEqual(e["forbidden_features"], ["gpl", "version3", "nonfree"])
        self.assertIn("minimal", e["release_hold"])


class WiringContracts(unittest.TestCase):
    def test_validator_and_contract_wired_into_ci(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("python3 scripts/windows-ffmpeg-validation-config.py", ci)
        self.assertIn("python3 scripts/test-windows-ffmpeg-validation-config.py", ci)

    def test_no_unsigned_prototype_source_in_validation_inputs(self):
        for relative in (
            "packaging/windows/validation/vcpkg.json",
            "packaging/windows/validation/vcpkg-configuration.json",
            "packaging/windows/validation/triplets/x64-windows-iris-dynamic.cmake",
        ):
            text = (ROOT / relative).read_text()
            for marker in ("db8537d8", "9.1.0", "ffmpeg-sys-next/git", "git+"):
                self.assertNotIn(marker, text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
