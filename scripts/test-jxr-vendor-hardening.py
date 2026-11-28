#!/usr/bin/env python3
"""Source-level regression for the vendored JPEG XR decoder's 2026 fixes.

The Rust tests exercise valid decode and locally crafted failures; these guards
prove that the specific upstream memory-safety hunks remain in the vendored C
sources. A changed guard is a defect, not a version difference.
"""

from pathlib import Path
import unittest

root = Path(__file__).resolve().parents[1]
vendor = root / "vendor/jpegxr"


def read(relative):
    return (vendor / relative).read_text(encoding="utf-8", errors="surrogateescape")


class JxrVendorHardening(unittest.TestCase):
    def test_upstream_security_hunks_are_present(self):
        strdec = read("jxrlib/image/decode/strdec.c")
        segdec = read("jxrlib/image/decode/segdec.c")
        glue = read("jxrlib/jxrgluelib/JXRGlueJxr.c")
        guards = (
            (
                strdec,
                """    /* Upstream d6e5f8d: reject an unterminated 8-byte signature before strstr. */
    if(szMS[7] != 0)
        return ICERR_ERROR;
    FailIf(szMS != (U8 *) strstr((char *) szMS, "WMPHOTO"), WMP_errUnsupportedFormat);
""",
            ),
            (
                segdec,
                """    /* Upstream debe47a: reject an out-of-range significance-run bin index. */
    if (iMaxRun < 0 || iMaxRun > 14) {
        return 0;
    }
    Int iIndex;
""",
            ),
            (
                strdec,
                """        default:
            /* Upstream ba86c04: reject an out-of-range bit-depth table index. */
            if(pII->bdBitDepth < BD_1 || pII->bdBitDepth >= BDB_MAX)
                return ICERR_ERROR;
            cStrideX = (pII->cBitsPerUnit >> 3) / cbChannels[pII->bdBitDepth];
            break;
""",
            ),
            (
                strdec,
                """        /** zero out the transform coefficients (pull this out to once per MB row) **/
        /* Upstream 190078d: reject a missing coefficient buffer before memset. */
        if (pSC->p1MBbuffer[0] == NULL)
            return ICERR_ERROR;
        memset(pSC->p1MBbuffer[0], 0, sizeof(PixelI) * 16 * 16 * pSC->cmbWidth);
""",
            ),
            (
                glue,
                """        case WMP_tagTransformation:
            FailIf(1 != uCount, WMP_errUnsupportedFormat);
            /* Upstream 7d2d462: fail closed instead of asserting on a bad enum. */
            FailIf(uValue >= O_MAX, WMP_errUnsupportedFormat);
""",
            ),
        )
        for source, guard in guards:
            self.assertEqual(source.count(guard), 1, f"missing or altered guard: {guard.splitlines()[1]}")

    def test_local_metadata_guard_is_preserved(self):
        text = read("jxrlib/jxrgluelib/JXRMeta.c")
        self.assertIn("IrisJxrMetadataStreamBounds", text)

    def test_provenance_and_ci_guard(self):
        provenance = read("IRIS-VENDOR.md")
        for commit in (
            "cb97e6d",
            "d6e5f8d19d452d1191c329614077f0fecab933c6",
            "debe47a27c1ae37626fae9b8369db93e47d0aa19",
            "ba86c041febb1472551681bae0d59a62e2e42072",
            "190078d2c4d8fcc9e4660baf0f65c960fa95dd21",
            "7d2d462f6ca2d8395dde8b33883a9751299d6144",
        ):
            self.assertIn(commit, provenance, f"provenance missing {commit}")
        ci = (root / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        self.assertIn("run: python3 scripts/test-jxr-vendor-hardening.py", ci)


if __name__ == "__main__":
    unittest.main()
