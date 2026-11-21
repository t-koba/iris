#!/usr/bin/env python3
"""Production Windows FFmpeg provenance contract.

A CI-green test-only prototype exists under prototype/windows-ffmpeg, but it
builds an unsigned FFmpeg git commit (db8537d8) with an unsigned
ffmpeg-sys-next 9.1.0 source override. Those sources are investigation
evidence only and must never enter a product release. Production approves the
signature-verified FFmpeg 8.1.3 source (packaging/linux/ffmpeg.json) and the
locked crates.io bindings ffmpeg-next 9.0.0 / ffmpeg-sys-next 9.0.1, whose
ffmpeg_8_1 ABI (62.28) matches that source.

The approved production Windows runtime plan is packaging/windows/ffmpeg.json;
this contract checks it resolves to the approved source and locked bindings, and
that the recorded binding ABI matches the approved source without a crate cache.
This checks the dependency route and the boundary, not native execution: the
native Windows build, DLL/PDFium closure, notices and clean-install preview
smoke still require a Windows runner via exact-SHA CI before release.yml may
ship a product build. Until then the Windows release variant stays minimal and
the release policy fails closed. The constants here are config evidence, not a
claim that a release artifact was built or that a codec is safe.
"""

from pathlib import Path
import json
import os
import re
import unittest

root = Path(__file__).resolve().parents[1]
registry_source = "registry+https://github.com/rust-lang/crates.io-index"

# Exact locked bindings approved for every Iris product build.
bindings = {
    "ffmpeg-next": (
        "9.0.0",
        "6380599799e175191eb7ffe82c97f36a2a90a36cbc54c738a903e5287d7f516a",
    ),
    "ffmpeg-sys-next": (
        "9.0.1",
        "c6f932d8c1cede016c65d4f1435f323369c99c74df55143b18676aa10e7603f9",
    ),
}

# Unsigned prototype pins, allowed only under prototype/ and the prototype workflow.
prototype_pins = ("db8537d8", "5aa9dc31", "x64-windows-prototype")

production_files = (
    ".github/workflows/release.yml",
    ".github/workflows/ci.yml",
    "scripts/package-windows.ps1",
    "scripts/release-audit.py",
    "scripts/release-policy.py",
    "scripts/release-policy.json",
    "Cargo.toml",
    "Cargo.lock",
    "packaging/windows/ffmpeg.json",
)


def read(relative):
    return (root / relative).read_text(encoding="utf-8")


def package_block(lock_text, name):
    pattern = (
        r"\[\[package\]\]\nname = \"" + re.escape(name) + r"\"\n"
        r"(.*?)(?=\n\[\[package\]\]|\Z)"
    )
    match = re.search(pattern, lock_text, re.S)
    return match.group(0) if match else ""


def patch_section():
    match = re.search(r"\[patch\.crates-io\]\n(.*?)(?=\n\[|\Z)", read("Cargo.toml"), re.S)
    return match.group(1) if match else ""


def cached_sys_source():
    homes = []
    cargo_home = os.environ.get("CARGO_HOME")
    if cargo_home:
        homes.append(Path(cargo_home))
    homes.append(Path.home() / ".cargo")
    for home in homes:
        for candidate in home.glob("registry/src/*/ffmpeg-sys-next-9.0.1/build.rs"):
            if candidate.is_file():
                return candidate.read_text(encoding="utf-8", errors="replace")
    return None


class WindowsFfmpegProvenance(unittest.TestCase):
    def test_lock_pins_approved_registry_bindings(self):
        lock_text = read("Cargo.lock")
        for name, (version, checksum) in bindings.items():
            block = package_block(lock_text, name)
            self.assertTrue(block, f"Cargo.lock has no {name} package block")
            self.assertIn(f'version = "{version}"', block)
            self.assertIn(f'source = "{registry_source}"', block)
            self.assertIn(f'checksum = "{checksum}"', block)
            self.assertNotIn("git+", block)
            self.assertNotIn('source = "path"', block)

    def test_no_unsigned_binding_override(self):
        patch = patch_section().lower()
        for name in bindings:
            self.assertNotIn(name, patch, f"{name} is overridden in [patch.crates-io]")
        manifests = [root / "Cargo.toml"]
        manifests.extend(sorted((root / "crates").glob("*/Cargo.toml")))
        for manifest in manifests:
            text = manifest.read_text(encoding="utf-8")
            for name in bindings:
                self.assertIsNone(
                    re.search(
                        re.escape(name) + r'\s*=\s*\{[^}]*\b(path|git)\s*=',
                        text,
                    ),
                    f"{manifest.relative_to(root)} overrides {name} to path/git",
                )

    def test_prototype_pins_confined_to_prototype(self):
        for relative in production_files:
            text = read(relative)
            for pin in prototype_pins:
                self.assertNotIn(
                    pin, text, f"unsigned prototype pin {pin} leaked into {relative}"
                )

    def test_approved_ffmpeg_target_is_signature_verified(self):
        policy = json.loads(read("packaging/linux/ffmpeg.json"))
        self.assertEqual(policy["version"], "8.1.3")
        self.assertEqual(policy["avcodec_version"], [62, 28, 103])
        for field in ("url", "sha256", "signature_url", "key_url", "fingerprint"):
            self.assertTrue(policy.get(field), f"approved pin missing {field}")
        flags = policy["configure_flags"]
        for forbidden in ("--disable-gpl", "--disable-version3", "--disable-nonfree"):
            self.assertIn(forbidden, flags)
        self.assertIn("--enable-shared", flags)
        self.assertIn("--disable-static", flags)

    def test_binding_abi_matches_approved_source(self):
        # Cache-independent semantic guard: the ABI declared by the locked
        # binding must equal the approved FFmpeg source's avcodec major/minor.
        # This runs in CI, where no crate source is cached at this step.
        plan = json.loads(read("packaging/windows/ffmpeg.json"))
        approved = json.loads(read(plan["ffmpeg"]["source"]))
        abi = plan["binding_abi"]
        self.assertEqual(abi["ffmpeg_8_1"], approved["avcodec_version"][:2])
        self.assertIn(bindings["ffmpeg-sys-next"][0], abi["declared_by"])

    def test_cached_binding_source_agrees_with_recorded_abi(self):
        # Supplementary byte-level check when the crate source is present;
        # skipping here no longer removes the CI-effective guard above.
        plan = json.loads(read("packaging/windows/ffmpeg.json"))
        major, minor = plan["binding_abi"]["ffmpeg_8_1"]
        source = cached_sys_source()
        if source is None:
            self.skipTest("ffmpeg-sys-next source not cached; ABI asserted from checked-in data")
        self.assertIn('("ffmpeg_8_1", %d, %d)' % (major, minor), source)

    def test_production_plan_matches_approved_pins(self):
        plan = json.loads(read("packaging/windows/ffmpeg.json"))

        # The FFmpeg source is referenced, not duplicated: it must resolve to
        # the approved signature-verified 8.1.3 pin.
        self.assertEqual(plan["ffmpeg"]["source"], "packaging/linux/ffmpeg.json")
        approved = json.loads(read(plan["ffmpeg"]["source"]))
        self.assertEqual(approved["version"], "8.1.3")
        self.assertEqual(approved["avcodec_version"], [62, 28, 103])
        for field in ("url", "sha256", "signature_url", "key_url", "fingerprint"):
            self.assertTrue(approved.get(field))

        # Bindings must be the exact locked registry crates the plan claims.
        self.assertEqual(plan["bindings"]["ffmpeg-next"], bindings["ffmpeg-next"][0])
        self.assertEqual(plan["bindings"]["ffmpeg-sys-next"], bindings["ffmpeg-sys-next"][0])

        # The vcpkg baseline's port builds 9.0.2; the plan must record that
        # verified mismatch so a build knows it has to override to the 8.1.3
        # tarball rather than promoting an unsigned git commit.
        self.assertRegex(plan["vcpkg"]["baseline"], r"^[0-9a-f]{40}$")
        self.assertEqual(plan["vcpkg"]["ffmpeg_port_version"], "9.0.2")
        self.assertEqual(plan["vcpkg"]["ffmpeg_port_revision"], 6)
        self.assertNotEqual(plan["vcpkg"]["ffmpeg_port_version"], approved["version"])
        self.assertIn("8.1.3", plan["vcpkg"]["constraint"])

        # PDFium must match the production chromium/8138 release and require
        # attestation; the Windows asset identity is pinned, not computed.
        fetch = read("scripts/fetch-pdfium-runtime.sh")
        self.assertIn(
            'pdfium_release="' + plan["pdfium"]["release"] + '"', fetch
        )
        self.assertIn(
            'pdfium_version="' + plan["pdfium"]["version"] + '"', fetch
        )
        self.assertEqual(plan["pdfium"]["asset"], "pdfium-win-x64.tgz")
        self.assertRegex(plan["pdfium"]["sha256"], r"^[0-9a-f]{64}$")
        self.assertGreater(plan["pdfium"]["bytes"], 0)
        self.assertTrue(plan["pdfium"]["attestation_required"])

    def test_windows_attestation_pin_matches_linux_contract(self):
        # The Linux staging script is authoritative: the Windows plan must pin
        # the same bundle, identity and source, with no silent drift or removal.
        fetch = read("scripts/fetch-pdfium-runtime.sh")
        commit = self.linux_pin("pdfium_commit")
        contract = dict(
            attestation_asset="pdfium-attestation.json",
            attestation_bytes=int(self.linux_pin("pdfium_attestation_bytes")),
            attestation_sha256=self.linux_pin("pdfium_attestation_sha256"),
            repo=self.linux_pin("pdfium_repo"),
            signer_workflow=self.linux_pin("pdfium_signer_workflow"),
            signer_digest=commit,
            source_digest=commit,
            source_ref=self.linux_pin("pdfium_source_ref"),
        )
        self.assertIn('pdfium-attestation.json', fetch)
        self.assertIn('attestation verify "$tarball"', fetch)
        self.assertIn('--signer-workflow "$pdfium_signer_workflow"', fetch)
        self.assertIn('--source-digest "$pdfium_commit"', fetch)
        literals = dict(contract, attestation_bytes=18267)
        literals.update(
            attestation_sha256="7526f4b934ab498d6206ac7f3d78dd4ce8e0f7d3175227ab3f4ecc255d950ad7",
            repo="bblanchon/pdfium-binaries",
            signer_workflow="github.com/bblanchon/pdfium-binaries/.github/workflows/build-all.yml",
            signer_digest="0eb94b28c2252d2baae3dfe15dc6b237131d8cd8",
            source_digest="0eb94b28c2252d2baae3dfe15dc6b237131d8cd8",
            source_ref="refs/heads/master",
        )
        for field, value in literals.items():
            self.assertEqual(contract[field], value, f"Linux contract {field} drifted")
        plan = json.loads(read("packaging/windows/ffmpeg.json"))["pdfium"]
        for field, value in contract.items():
            self.assertEqual(plan.get(field), value,
                             f"Windows PDFium {field} differs from the Linux contract")

    def linux_pin(self, name):
        fetch = read("scripts/fetch-pdfium-runtime.sh")
        match = re.search(rf'^{re.escape(name)}=(?:"([^"]+)"|([^\s#]+))$',
                          fetch, re.MULTILINE)
        self.assertIsNotNone(match, f"Linux contract pin {name} missing")
        return match.group(1) or match.group(2)

    def test_windows_release_hold_is_explicit(self):
        release = read(".github/workflows/release.yml")
        self.assertIn("IRIS_WINDOWS_RELEASE_HELD", release)
        self.assertNotIn("./scripts/package-windows.ps1", release)
        policy = json.loads(read("scripts/release-policy.json"))
        self.assertEqual(policy["required_variants"]["windows"], "product")
        self.assertIn("python scripts/release-policy.py", release)

    def test_contract_wired_into_ci(self):
        ci = read(".github/workflows/ci.yml")
        self.assertIn("python3 scripts/test-windows-ffmpeg-provenance.py", ci)


if __name__ == "__main__":
    unittest.main()
