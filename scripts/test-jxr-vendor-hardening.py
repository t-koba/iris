#!/usr/bin/env python3
"""Source-level regression for the vendored JPEG XR decoder's 2026 fixes.

The Rust tests exercise valid decode and locally crafted failures; these guards
prove that the specific upstream memory-safety hunks remain in the vendored C
sources. A changed guard is a defect, not a version difference.
"""

from pathlib import Path
import re
import unittest

root = Path(__file__).resolve().parents[1]
vendor = root / "vendor/jpegxr"

SECOND_BATCH_COMMITS = (
    "544a0ce8f7d11d24c2e73ee52ec6c5073c975b57",
    "c7f0ff5d8d27ff579fd4075b7e6e41aa5afbe3e2",
    "9f5616abf3e50ebf28a734006f420853492e2d87",
    "0d92eb5ab4fca48c16fee672f66554338117251b",
    "4b40a579d4838b8f67ed545b252fff8b06db0c9a",
    "9bf026d7b33d4331cd5e47b94c33bd010cc9a197",
    "5d932789cfeaf32298a7c280d819d6fc0a6e5f6b",
    "0e9a58411bc876a2737963c54748cfc40744a326",
)


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

    def test_second_upstream_security_batch_is_present(self):
        strdec = read("jxrlib/image/decode/strdec.c")
        segdec = read("jxrlib/image/decode/segdec.c")
        strcodec = read("jxrlib/image/sys/strcodec.c")
        glue = read("jxrlib/jxrgluelib/JXRGlueJxr.c")
        factory = read("jxrlib/jxrgluelib/JXRGlue.c")
        guards = (
            (
                strcodec,
                """    /* Upstream 544a0ce: no tile array means there is nothing to free. */
    if(pSC->pTile == NULL)
        return;
""",
            ),
            (
                strcodec,
                """Void freeQuantizer(CWMIQuantizer * pQuantizer[MAX_CHANNELS])
{
    if(pQuantizer != NULL && pQuantizer[0] != NULL)
        free(pQuantizer[0]);
}
""",
            ),
            (
                glue,
                """    linesperMBRow = 16 / cThumbnailScale;
    /* Upstream c7f0ff5 / 0d92eb5: reject a thumbnail scale that would divide by zero. */
    FailIf(0 == linesperMBRow, WMP_errFail);
""",
            ),
            (
                strdec,
                """Cleanup:
    if (err != WMP_errSuccess && pSC != NULL) {
        /* Upstream 9f5616a: release a partially initialized decoder on failure. */
        pSC->m_pNextSC = pNextSC;
        StrDecTerm(pSC);
        free(pSC->m_pNextSC);
        free(pSC);
    }
    return WMP_errSuccess == err ? ICERR_OK : ICERR_ERROR;
""",
            ),
            (
                factory,
                """Cleanup:
    /* Upstream 9f5616a: release a decoder whose initialization failed. */
    if (pDecoder && err != WMP_errSuccess)
        pDecoder->Release(ppDecoder);
    return err;
""",
            ),
            (
                glue,
                """            assert((DPKVT_BYREF | DPKVT_UI1) == pID->WMP.sDescMetadata.pvarCaption.vt);

            // Change type from C-style byte array to LPWSTR
            assert((U8*)pID->WMP.sDescMetadata.pvarCaption.VT.pwszVal ==
                pID->WMP.sDescMetadata.pvarCaption.VT.pbVal);
            //  make sure null term (ReadPropvar allocated enough space for this)
            pID->WMP.sDescMetadata.pvarCaption.VT.pwszVal[uCount/sizeof(U16)] = 0;
""",
            ),
            (
                segdec,
                """    if((pSC->WMISCP.bfBitstreamFormat != SPATIAL) && (pSC->pTile[pSC->cTileColumn].cBitsLP > 0)) {  // MB-based LP QP index
        pMBInfo->iQIndexLP = decodeQPIndex(pIO, pSC->pTile[pSC->cTileColumn].cBitsLP);
        /* Upstream 9bf026d: reject an out-of-range MB-based LP QP index. */
        if (pMBInfo->iQIndexLP >= pSC->pTile[pSC->cTileColumn].cNumQPLP)
            return ICERR_ERROR;
    }
""",
            ),
            (
                strdec,
                """    /* Upstream 5d93278: keep dimensions small enough for stride and buffer arithmetic. */
    FailIf(pII->cWidth > (1 << 28) || pII->cHeight > (1 << 28), WMP_errUnsupportedFormat);
""",
            ),
            (
                strdec,
                """    /* Upstream 5d93278: reject a plane larger than the caller's image dimensions. */
    if(pII->cWidth && pII->cHeight && (cII.cWidth > pII->cWidth || cII.cHeight > pII->cHeight))
        return ICERR_ERROR;
""",
            ),
            (
                glue,
                """            /* Upstream 0e9a584: rewind before capturing the marker for a fresh decode. */
            Call(pID->WMP.wmiSCP.pWStream->SetPos(pID->WMP.wmiSCP.pWStream, pID->WMP.wmiDEMisc.uImageOffset));
            Call(pID->WMP.wmiSCP.pWStream->GetPos(pID->WMP.wmiSCP.pWStream, &(pID->WMP.cMarker)));
""",
            ),
            (
                glue,
                """        if (linesperMBRow * (cMBRow - 1) >= (U32) pID->WMP.cLinesCropped + pID->WMP.wmiI.cROIHeight) {
            FailIf(ICERR_OK != ImageStrDecTerm(pID->WMP.ctxSC), WMP_errFail);
            pID->WMP.DecoderCurrMBRow = 0;
        }
        else
            pID->WMP.DecoderCurrMBRow = cMBRow; // Set to next possible MBRow that is decodable
""",
            ),
            (
                glue,
                """        if (linesperMBRow * (cMBRow - 1) >= (U32) pID->WMP.cLinesCropped + pID->WMP.wmiI.cROIHeight) {
            FailIf(ICERR_OK != ImageStrDecTerm(pID->WMP.ctxSC_Alpha), WMP_errFail);
            pID->WMP.DecoderCurrAlphaMBRow = 0;
        }
        else
            pID->WMP.DecoderCurrAlphaMBRow = cMBRow; // Set to next possible MBRow that is decodable
""",
            ),
            (
                glue,
                """Cleanup:
    /* Upstream 0e9a584: a failed reentrant decode leaves no usable decoder state. */
    if (err != WMP_errSuccess) {
        pID->WMP.DecoderCurrMBRow = 0;
        pID->WMP.DecoderCurrAlphaMBRow = 0;
    }
    return err;
""",
            ),
        )
        for source, guard in guards:
            self.assertEqual(source.count(guard), 1, f"missing or altered guard: {guard.splitlines()[1]}")
        self.assertNotIn("Confirm null-term", glue)
        self.assertIn(
            """    if(pTile->cBitsHP == 0 && pTile->cNumQPHP > 1) // use LP QP
        pMBInfo->iQIndexHP = pMBInfo->iQIndexLP;
    if (pMBInfo->iQIndexLP >= pTile->cNumQPLP || pMBInfo->iQIndexHP >= pTile->cNumQPHP)
        return ICERR_ERROR;
""",
            segdec,
        )

    def test_local_metadata_and_stream_guards_are_preserved(self):
        meta = read("jxrlib/jxrgluelib/JXRMeta.c")
        self.assertIn("IrisJxrMetadataStreamBounds", meta)
        self.assertIn(
            """    // uCount is file-controlled. The allocation below adds two bytes in U32
    // arithmetic, so reject counts that could wrap before allocating. This is
    // an intrinsic limit of that allocation, not a policy cap on metadata.
    FailIf(uCount > (U32) -3, WMP_errInvalidArgument);
""",
            meta,
        )
        self.assertIn("Call(IrisJxrMetadataStreamBounds(uCount, uValue, pWS));", meta)
        stream = read("jxrlib/image/sys/windowsmediaphoto.h")
        self.assertIn("ERR (*GetSize)(struct WMPStream* me, size_t* pcbSize);", stream)
        strcodec = read("jxrlib/image/sys/strcodec.c")
        for impl in ("GetSizeWS_File", "GetSizeWS_Memory", "GetSizeWS_List"):
            self.assertIn(impl, strcodec, f"stream size query missing {impl}")

    def test_cve_traceability_matches_imported_first_batch(self):
        provenance = read("IRIS-VENDOR.md")
        mappings = (
            ("cb97e6d", "CVE-2026-129857"),
            ("d6e5f8d19d452d1191c329614077f0fecab933c6", "CVE-2026-129858"),
            ("debe47a27c1ae37626fae9b8369db93e47d0aa19", "CVE-2024-58407"),
            ("ba86c041febb1472551681bae0d59a62e2e42072", "CVE-2024-58408"),
            ("190078d2c4d8fcc9e4660baf0f65c960fa95dd21", "CVE-2026-129859"),
            ("7d2d462f6ca2d8395dde8b33883a9751299d6144", "CVE-2026-129860"),
        )
        for commit, cve in mappings:
            self.assertIn(f"`{commit}` is {cve}", provenance, f"missing mapping {commit}")
        # Only the six first-batch identifiers may appear anywhere, and no
        # second-batch commit may share a line with a CVE identifier.
        expected = {cve for _, cve in mappings}
        found = set(re.findall(r"CVE-\d{4}-\d+", provenance))
        self.assertEqual(
            found, expected, f"unexpected CVE identifiers: {sorted(found - expected)}"
        )
        for commit in SECOND_BATCH_COMMITS:
            for line in provenance.splitlines():
                if commit in line:
                    self.assertNotRegex(
                        line, r"CVE-\d{4}-\d+", f"CVE attributed to {commit}"
                    )

    def test_provenance_and_ci_guard(self):
        provenance = read("IRIS-VENDOR.md")
        for commit in (
            "cb97e6d",
            "d6e5f8d19d452d1191c329614077f0fecab933c6",
            "debe47a27c1ae37626fae9b8369db93e47d0aa19",
            "ba86c041febb1472551681bae0d59a62e2e42072",
            "190078d2c4d8fcc9e4660baf0f65c960fa95dd21",
            "7d2d462f6ca2d8395dde8b33883a9751299d6144",
            *SECOND_BATCH_COMMITS,
        ):
            self.assertIn(commit, provenance, f"provenance missing {commit}")
        ci = (root / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        self.assertIn("run: python3 scripts/test-jxr-vendor-hardening.py", ci)


if __name__ == "__main__":
    unittest.main()
