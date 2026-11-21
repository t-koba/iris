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



class ImportAllowlistContracts(unittest.TestCase):
    """CRT and OS import allowlists stay separate, bounded and checkout-bound."""

    def setUp(self):
        self.os_entries = mod.read_dll_allowlist(mod.OS_IMPORT_ALLOWLIST)
        self.vc_entries = mod.read_dll_allowlist(mod.VC_IMPORT_ALLOWLIST)

    def test_production_allowlists_are_separate_and_bounded(self):
        self.assertTrue(mod.validate_import_allowlists(self.os_entries, self.vc_entries))
        for entries in (self.os_entries, self.vc_entries):
            self.assertTrue(entries)
            self.assertLessEqual(len(entries), mod.ALLOWLIST_LIMIT)
        self.assertFalse(set(self.os_entries) & set(self.vc_entries))

    def test_rejects_crt_in_os_allowlist(self):
        for name in mod.VC_SELF_TEST_DLLS + ("vcruntime140d.dll", "api-ms-win-crt-math-l1-1-0.dll"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                mod.validate_import_allowlists(self.os_entries + [name], self.vc_entries)

    def test_rejects_missing_vc_entry(self):
        for name in mod.VC_SELF_TEST_DLLS:
            with self.subTest(name=name), self.assertRaises(ValueError):
                mod.validate_import_allowlists(
                    self.os_entries, [entry for entry in self.vc_entries if entry != name])

    def test_rejects_staged_names_in_either_allowlist(self):
        for name in ("iris.exe", "pdfium.dll", "avcodec-62.dll"):
            for os_entries, vc_entries in ((self.os_entries + [name], self.vc_entries),
                                           (self.os_entries, self.vc_entries + [name])):
                with self.subTest(name=name), self.assertRaises(ValueError):
                    mod.validate_import_allowlists(os_entries, vc_entries)

    def test_rejects_unbounded_or_unsafe_entry(self):
        for relative in (mod.OS_IMPORT_ALLOWLIST, mod.VC_IMPORT_ALLOWLIST):
            with self.subTest(relative=relative):
                entries = mod.read_dll_allowlist(relative)
                with self.assertRaises(ValueError):
                    mod.validate_import_allowlists(entries * (mod.ALLOWLIST_LIMIT + 1),
                                                   self.vc_entries)
        with self.assertRaises(OSError):
            mod.read_dll_allowlist(mod.VC_IMPORT_ALLOWLIST.replace(
                "windows-vc-redist-allowlist.txt", "missing-vc-allowlist.txt"))

    def test_build_records_allowlist_inputs(self):
        evidence = mod.build()
        for key in ("os_allowlist_sha256", "vc_allowlist_sha256"):
            self.assertEqual(len(evidence[key]), 64)
            self.assertTrue(all(c in "0123456789abcdef" for c in evidence[key]))
        self.assertEqual(evidence["os_allowlist_entries"], len(self.os_entries))
        self.assertEqual(evidence["vc_allowlist_entries"], len(self.vc_entries))


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


class PackagingHoldContracts(unittest.TestCase):
    def test_release_hold_requires_inventory_packager(self):
        cfg = (ROOT / "scripts/windows-ffmpeg-validation-config.py").read_text()
        pkg = (ROOT / "scripts/package-windows.ps1").read_text()
        self.assertIn("package-windows.ps1 must consume the bounded exact-SHA product inventory", cfg)
        self.assertIn("package-windows.ps1 must not rebuild a reduced feature variant", cfg)
        for token in ("IRIS_PRODUCT_STAGE", "IRIS_PRODUCT_INVENTORY", "IRIS_PRODUCT_ZIP",
                      "pwsh scripts/windows-product-package.ps1"):
            self.assertIn(token, pkg)
        self.assertNotIn("--no-default-features", pkg)
        self.assertNotIn('"--features", "minimal"', pkg)


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
