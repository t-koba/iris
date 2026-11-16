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
in-memory invalid-input regression. The Rust API, build script and C codec
sources are unchanged. Remove the patch when upstream includes both fixes.

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
