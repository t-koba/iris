# jpegxr 0.3.1

Source: the crates.io `jpegxr` 0.3.1 package from
<https://github.com/brion/jpegxr>, upstream revision
`3e8b298304cd8a42e773afb0af5cf57bd49549c4` (package VCS metadata); crates.io
archive SHA-256 `20a0328c45484d19035d80b72eaadaefa374ff3f4b07737b6ad426587c890fbf`.
License: the crate metadata declares BSD-3-Clause, but the bundled jxrlib C sources and `license.md` text are BSD-2-Clause (two conditions, no non-endorsement clause). See `license.md` and the notices in the C sources.

This temporary Cargo patch changes the build dependency from bindgen 0.69
to 0.73.2 with the same runtime-only features used by Iris's FFmpeg bindings.
This avoids enabling the unused prettyplease formatter (its syn version can
differ from bindgen's). Older bindgen can generate an opaque
`tagPKImageDecode` with newer libclang on Ubuntu, hiding the decoder's function
pointers. The Rust wrapper also guards the C decoder during construction so
failed creation or initialization releases any allocated handle, with an
in-memory invalid-input regression. Remove the patch when upstream includes
both fixes.

Local hardening (no upstream code imported, license unchanged): the vendored
jxrlib 1.1 metadata reader allocates a file-controlled count plus two bytes in
U32 arithmetic. A count near `U32::MAX` can wrap that allocation size. The
count is now rejected before allocation: the stream bounds are validated
before the buffer is allocated, and a test observes the metadata allocation
count to prove that a stream overrun allocates zero buffers. `WMPStream`
gained a size query implemented for its file, memory, list and Rust-reader
implementations so that
inline metadata must fit in the input stream. `JXRMeta.c` carries the guard,
`strcodec.c`/`strcodec.h` and `windowsmediaphoto.h` carry the stream query, and
`build.rs`/`src/lib.rs` expose the Rust stream size callback. A test-only C
probe exercises the production guard with boundary counts without adding any
crafted JPEG-XR bytes. This is a local source fix, not a claim of end-to-end
Iris exploitability or a replacement of the vendored decoder.

Imported upstream safety fixes (BSD-2-Clause text in `license.md` and C headers; crate metadata declares BSD-3-Clause, same upstream project):
the mircomir/jxrlib fixes for the six damaged-file memory bugs reported in 2026.
The metadata-count wrap is already covered by the local guard above (upstream
`cb97e6d`). The remaining five minimal hunks are ported here with their commit
IDs: `d6e5f8d19d452d1191c329614077f0fecab933c6` rejects an unterminated
8-byte WMPHOTO signature before `strstr`;
`debe47a27c1ae37626fae9b8369db93e47d0aa19` bounds the significance-run table
index; `ba86c041febb1472551681bae0d59a62e2e42072` bounds the bit-depth table
index; `190078d2c4d8fcc9e4660baf0f65c960fa95dd21` rejects a missing coefficient
buffer before `memset`; and `7d2d462f6ca2d8395dde8b33883a9751299d6144` turns a
debug-only orientation assertion into a hard format error. Only these security
hunks were imported; unrelated platform, version and cosmetic upstream changes
were not. `scripts/test-jxr-vendor-hardening.py` fails if any of them is absent
or altered. This is a source-level hardening of the existing vendored copy,
not a claim of end-to-end Iris exploitability or release readiness.

Second imported batch, from the 1.4.5-1.4.7 range of the same upstream
(BSD-2-Clause sources and license text, unchanged). The vendored tree is not
stock 1.4.4, so each hunk was adapted function-by-function rather than applied
as a wholesale patch. Only these eight security commits were imported:
`544a0ce8f7d11d24c2e73ee52ec6c5073c975b57` (NULL tile array and quantizer
guards), `c7f0ff5d8d27ff579fd4075b7e6e41aa5afbe3e2` and `0d92eb5ab4fca48c16fee672f66554338117251b` (short ID as
supplied; the same divide-by-zero guard in `PKImageDecode_Copy_WMP`),
`9f5616abf3e50ebf28a734006f420853492e2d87` (release partially initialized
decoders; `Release` receives `ppDecoder`),
`4b40a579d4838b8f67ed545b252fff8b06db0c9a` (caption type check and removal of
the dangling null-terminator assert, keeping the terminator write),
`9bf026d7b33d4331cd5e47b94c33bd010cc9a197` (lowpass MB QP index bound),
`5d932789cfeaf32298a7c280d819d6fc0a6e5f6b` (header dimension limit and plane
size validation), and `0e9a58411bc876a2737963c54748cfc40744a326` (reentrant
stream rewind and row-counter state). The vendored caption check already used
`==`, so no type change was needed there; upstream Pascal-style comments are
written as C comments. In `PKImageDecode_Copy_WMP` the vendored code uses
`FailIf` rather than the upstream explicit decode-failure branch, so the
row counters are reset in `Cleanup` on any failure; this preserves the same
re-init requirement without changing decoded output. README,
JXRVersion.h, license bytes, dependencies, release policy and parity
baselines are untouched, and the local WMPStream
size query and metadata-count guard above remain in force. Only these security
hunks were imported; unrelated upstream changes were not. The source-level
guards in `scripts/test-jxr-vendor-hardening.py` pin both batches and the local
guards.

The registry cache markers, duplicate manifest/license metadata, JS/Wasm tooling
and binary documentation are omitted. The 8.7 MiB upstream screenshot is replaced
by a 17 KiB synthetic fixture at `samples/panel-hdr.jxr`, preserving the metadata
asserted by the unchanged upstream Rust test (3440 x 1440, 128-bit RGBA float).
The synthetic image contains no screenshot or user data: every pixel is
RGBA `(0.25, 0.5, 2.0, 1.0)`. It was generated with the vendored jxrlib encoder
using `GUID_PKPixelFormat128bppRGBAFloat`, 96 DPI and zero-initialized
`CWMIStrCodecParam` with `cfColorFormat = YUV_444`, `bdBitDepth = BD_LONG`,
`bfBitstreamFormat = FREQUENCY`, `uAlphaMode = 2`,
`nLenMantissaOrShift = 13` and `nExpBias = 4`. Its SHA-256 is
`ea358a1d7d69ae1119bb6c99aa51717bc0995ae3dfdcf94fec3ca8bc85ed0b85`.
