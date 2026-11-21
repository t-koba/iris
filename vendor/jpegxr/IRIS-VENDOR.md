# jpegxr 0.3.1

Source: the crates.io `jpegxr` 0.3.1 package from
<https://github.com/brion/jpegxr>, upstream revision
`3e8b298304cd8a42e773afb0af5cf57bd49549c4` (package VCS metadata); crates.io
archive SHA-256 `20a0328c45484d19035d80b72eaadaefa374ff3f4b07737b6ad426587c890fbf`.
License: BSD-3-Clause; see `license.md` and the notices in the C sources.

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
