# POSIX native closure evidence

`scripts/posix-native-evidence.py` is the release producer for the two POSIX
native-closure sidecars. It never approves a license and never changes a release
file: it re-hashes the exact `dist/macos` or `dist/linux` stage, matches every
file to a rule in the platform mapping, inspects actual native dependencies with
`otool`/`ldd`, and refuses to write a sidecar if any file, dependency or license
bucket is unmapped. The generated sidecar is written under `audit/` and consumed
by `release-audit.py --require-native-evidence`, so the existing release-policy
gate still rejects a missing or inconsistent closure.

The two `*-mapping.json` files are the reviewed inputs for this step. They are
deliberately explicit rather than inferential:

- `ffmpeg-*` staged shared libraries get a review-disposition `LGPL-2.1-or-later`
  component with notice, source and relinking obligations, bound to the pinned
  signature-verified FFmpeg source and to the license/source documents actually
  staged in the product.
- The Iris executable's FFmpeg imports are recorded as a separate review
  component because the dynamic link itself is part of the shipped runtime.
- On macOS, pinned static liblzma is an additional allowed 0BSD `static`
  component, but only on a dylib whose staged `ffmpeg-runtime.json` reports
  canonical liblzma symbols and whose build pin/manifest hashes bind to that
  dylib; the producer re-runs `nm -gU` on the staged bytes. Mapping rules never
  guess the containing dylib list, and a stage without a verified static
  liblzma component is refused.
- Linux PDFium gets an allowed `BSD-3-Clause` component bound to the pinned
  bblanchon release and to the staged PDFium license/provenance documents.
- Unmapped system libraries are refused; a reviewed `system_components` entry
  records the license bucket, Debian owner package and obligation disposition
  for each permitted dependency. `libgcc_s.so.1` remains a scoped GCC-runtime
  exception already present in `release-policy.json`.
- Data, package and evidence files have explicit rules but no native component;
  their exact bytes are still inventoried in the closure artifact list.

Because the closure is produced from the stage bytes immediately before the
audit, a rebuilt or tampered stage yields different hashes and a different
sidecar. A reviewer sees the mapping, the generated sidecar and the review
report as release artifacts; no pre-approved or cross-run manifest is reused.
