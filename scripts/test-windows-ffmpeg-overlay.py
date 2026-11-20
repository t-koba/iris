#!/usr/bin/env python3
"""Offline contracts for the production Windows FFmpeg vcpkg overlay derivation."""

import hashlib
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("overlay", ROOT / "scripts/windows-ffmpeg-overlay.py")
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

GOOD_SRC = "/build/staged/ffmpeg-8.1.3-src"


def portfile(fetch=None):
    head = "# pinned vcpkg baseline portfile\n"
    tail = "\nif(SOURCE_PATH MATCHES \" \")\n  message(FATAL_ERROR \"spaces\")\nendif()\n"
    return head + (fetch if fetch is not None else mod.BASELINE_FETCH) + tail


class BaselineContracts(unittest.TestCase):
    def test_baseline_fetch_is_pinned_and_stable(self):
        self.assertEqual(
            hashlib.sha256(mod.BASELINE_FETCH.encode()).hexdigest(),
            "3cf059f69bd3a2ecf654cb7c6888ae579279357de06651837421eef7b7258548",
        )

    def test_baseline_is_the_9_0_2_github_fetch(self):
        self.assertIn("REPO ffmpeg/ffmpeg", mod.BASELINE_FETCH)
        self.assertIn('REF "n${VERSION}"', mod.BASELINE_FETCH)
        self.assertIn("vcpkg_from_github", mod.BASELINE_FETCH)

    def test_all_14_baseline_patches_recorded(self):
        patches = mod.baseline_patches()
        self.assertEqual(len(patches), 14)
        self.assertEqual(patches[0], "0003-fix-windowsinclude.patch")
        self.assertEqual(patches[-1], "0052-fix-disable-unstable-swscale-link.patch")
        self.assertTrue(all(name.endswith(".patch") for name in patches))


class DerivationContracts(unittest.TestCase):
    def test_derivation_replaces_fetch_with_verified_local_source(self):
        rewritten, evidence = mod.derive(portfile(), GOOD_SRC)
        self.assertIn('set(SOURCE_PATH "%s")' % GOOD_SRC, rewritten)
        self.assertNotIn("vcpkg_from_github", rewritten)
        self.assertNotIn("db8537d8", rewritten)
        self.assertIn("if(SOURCE_PATH MATCHES", rewritten)  # untouched tail preserved
        self.assertEqual(evidence["source_path"], GOOD_SRC)
        self.assertEqual(evidence["baseline_ffmpeg_version"], "9.0.2")
        self.assertEqual(evidence["production_ffmpeg_version"], "8.1.3")

    def test_evidence_records_unapplied_patches(self):
        _, evidence = mod.derive(portfile(), GOOD_SRC)
        self.assertEqual(len(evidence["baseline_patches_not_applied"]), 14)
        self.assertIn("unvalidated for 8.1.3", evidence["patch_note"])
        self.assertIn("exact-SHA Windows CI", evidence["patch_note"])

    def test_source_path_sha256_is_path_digest_not_fetch_digest(self):
        _, evidence = mod.derive(portfile(), GOOD_SRC)
        self.assertEqual(
            evidence["source_path_sha256"],
            hashlib.sha256(GOOD_SRC.encode()).hexdigest(),
        )
        self.assertNotEqual(evidence["source_path_sha256"],
                            evidence["baseline_fetch_sha256"])

    def test_portfile_digest_recorded(self):
        text = portfile()
        _, evidence = mod.derive(text, GOOD_SRC)
        self.assertEqual(evidence["baseline_portfile_sha256"],
                         hashlib.sha256(text.encode()).hexdigest())
        self.assertEqual(evidence["baseline_fetch_sha256"],
                         hashlib.sha256(mod.BASELINE_FETCH.encode()).hexdigest())

    def test_cli_derive_writes_portfile_and_evidence(self):
        with tempfile.TemporaryDirectory() as td:
            pf = Path(td) / "portfile.cmake"
            pf.write_text(portfile())
            out = Path(td) / "overlay" / "portfile.cmake"
            import subprocess
            r = subprocess.run(
                [sys.executable, str(ROOT / "scripts/windows-ffmpeg-overlay.py"),
                 "derive", str(pf), GOOD_SRC, str(out)],
                capture_output=True, text=True,
            )
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn('set(SOURCE_PATH "%s")' % GOOD_SRC, out.read_text())
            import json
            evidence = json.loads(out.with_suffix(".evidence.json").read_text())
            self.assertEqual(evidence["production_ffmpeg_version"], "8.1.3")


class FailClosedContracts(unittest.TestCase):
    def test_rejects_shape_drift(self):
        drifted = mod.BASELINE_FETCH.replace("HEAD_REF master", "HEAD_REF release/9.0")
        with self.assertRaises(ValueError):
            mod.derive(portfile(drifted), GOOD_SRC)

    def test_rejects_changed_patch_list(self):
        drifted = mod.BASELINE_FETCH.replace(
            "        0003-fix-windowsinclude.patch\n", "")
        with self.assertRaises(ValueError):
            mod.derive(portfile(drifted), GOOD_SRC)

    def test_rejects_two_fetches(self):
        with self.assertRaises(ValueError):
            mod.derive(portfile(mod.BASELINE_FETCH + mod.BASELINE_FETCH), GOOD_SRC)

    def test_rejects_missing_fetch(self):
        with self.assertRaises(ValueError):
            mod.derive("# no fetch here\n", GOOD_SRC)

    def test_rejects_relative_source_path(self):
        with self.assertRaises(ValueError):
            mod.derive(portfile(), "relative/src")

    def test_rejects_spaces_in_source_path(self):
        with self.assertRaises(ValueError):
            mod.derive(portfile(), "/build/staged path/ffmpeg-8.1.3-src")

    def test_rejects_shell_metacharacters_in_source_path(self):
        for bad in ("/build/$(id)/src", "/build/a;b/src", "/build/a`b/src", "/build/a'b/src"):
            with self.subTest(path=bad), self.assertRaises(ValueError):
                mod.derive(portfile(), bad)

    def test_rejects_unsigned_source_markers_in_portfile_tail(self):
        for marker in ("git+", "db8537d8", "vcpkg_from_git", "vcpkg_download_distfile"):
            with self.subTest(marker=marker), self.assertRaises(ValueError):
                mod.derive(portfile() + f"# injected {marker}\n", GOOD_SRC)

    def test_output_has_no_unsigned_or_gpl_markers(self):
        rewritten, _ = mod.derive(portfile(), GOOD_SRC)
        for marker in mod.SOURCE_UNSAFE + mod.REPLACEMENT_UNSAFE:
            self.assertNotIn(marker, rewritten)


class FeatureContracts(unittest.TestCase):
    def test_forbidden_features_rejected(self):
        for feature in ("gpl", "version3", "nonfree"):
            with self.subTest(feature=feature), self.assertRaises(ValueError):
                mod.validate_features(["avcodec", "avformat", feature])

    def test_lgpl_feature_set_accepted(self):
        selected = mod.validate_features(["avcodec", "avformat", "swscale", "network"])
        self.assertEqual(selected, ["avcodec", "avformat", "swscale", "network"])

    def test_evidence_declares_forbidden_features(self):
        _, evidence = mod.derive(portfile(), GOOD_SRC)
        self.assertEqual(evidence["forbidden_features"], ["gpl", "version3", "nonfree"])


class FullPortfileContracts(unittest.TestCase):
    def test_derivation_accepts_real_feature_gated_portfile_tail(self):
        # The pinned baseline gates GPL flags behind vcpkg features; they are
        # inert unless selected and must not make derive() reject the file.
        tail = (
            'if("gpl" IN_LIST FEATURES)\n'
            '    set(OPTIONS "${OPTIONS} --enable-gpl")\n'
            'endif()\n'
            'if("nonfree" IN_LIST FEATURES)\n'
            '    set(OPTIONS "${OPTIONS} --enable-nonfree")\n'
            'endif()\n'
        )
        rewritten, _ = mod.derive(portfile() + tail, GOOD_SRC)
        self.assertIn('set(SOURCE_PATH "%s")' % GOOD_SRC, rewritten)
        self.assertIn("--enable-gpl", rewritten)  # feature-gated, not enabled


class WiringContracts(unittest.TestCase):
    def test_contract_wired_into_ci(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("python3 scripts/test-windows-ffmpeg-overlay.py", ci)

    def test_overlay_module_has_no_network_or_git_invocation(self):
        src = (ROOT / "scripts/windows-ffmpeg-overlay.py").read_text()
        self.assertNotIn("subprocess", src)
        self.assertNotIn("urlopen", src)
        self.assertNotIn("requests", src)


if __name__ == "__main__":
    unittest.main(verbosity=2)
