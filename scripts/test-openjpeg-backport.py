#!/usr/bin/env python3
"""Guard-level patch-integrity evidence for the vendored OpenJPEG backport.

This checks that the two upstream decoder guards are present verbatim in the
vendored C sources. It is guard-presence evidence, not execution evidence: it
does not prove a runtime path reaches a guard.

In particular, the early-truncation rejection exercised by
``modern_image::tests::rejects_truncated_jpeg2000_codestream`` fails during SIZ
parsing, before ``opj_j2k_read_sod`` runs, so it does not exercise the tile-part
index guard. No advisory proof of concept was reproduced in Iris.

The upstream patch SHA-256 values are from the raw GitHub patch responses used
to verify the guards (external-untrusted evidence, retained as fetch receipts).
"""

from pathlib import Path
import unittest

root = Path(__file__).resolve().parents[1]
jp2 = (root / "vendor/openjpeg-sys/vendor/src/lib/openjp2/jp2.c").read_text(encoding="utf-8")
j2k = (root / "vendor/openjpeg-sys/vendor/src/lib/openjp2/j2k.c").read_text(encoding="utf-8")
vendor_record = (root / "vendor/openjpeg-sys/IRIS-VENDOR.md").read_text(encoding="utf-8")
modern_image = (root / "crates/thumb-preview/src/modern_image.rs").read_text(encoding="utf-8")

CVE_COMMIT = "f809b80c67717c152a5ad30bf06774f00da4fd2d"
CVE_PATCH_SHA256 = "13d5afb17d56f5cba3644585f08c29b1b2831c9cd54db73d1fc17b8cebdbe921"
OSV_COMMIT = "91d08b11a72764f6d199f32fa0c1b1abd4edc2ad"
OSV_PATCH_SHA256 = "233bf4c149d7740dd802c36c3e78e80352454b27d5579c1a4ef3260c2f4e90a1"


class OpenJpegBackportIntegrity(unittest.TestCase):
    def test_jp2_header_guard_present(self):
        # CVE-2025-54874: color space is set only after a successful header read.
        self.assertIn("if (ret && p_image && *p_image) {", jp2)

    def test_tile_part_guard_present(self):
        # OSV-2025-219: both end_header and end_pos writes in opj_j2k_read_sod
        # sit inside one null-and-bounds guard.
        self.assertIn(
            "l_cstr_index->tile_index[p_j2k->m_current_tile_number].tp_index &&",
            j2k,
        )
        self.assertIn(
            "l_current_tile_part < l_cstr_index->tile_index[p_j2k->m_current_tile_number].nb_tps) {",
            j2k,
        )

    def test_upstream_fix_identity_recorded(self):
        for value in (CVE_COMMIT, CVE_PATCH_SHA256, OSV_COMMIT, OSV_PATCH_SHA256):
            self.assertIn(value, vendor_record)

    def test_decoder_success_and_rejection_checks_retained(self):
        self.assertIn("fn decodes_jpeg2000_preview()", modern_image)
        self.assertIn("fn rejects_truncated_jpeg2000_codestream()", modern_image)


if __name__ == "__main__":
    unittest.main()
