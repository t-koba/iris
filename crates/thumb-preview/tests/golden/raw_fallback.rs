use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::thumbnail::ThumbnailHint;
use iris_thumb_preview::pipeline::ThumbnailPipeline;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

const TIFF_MAGIC_STANDARD: u16 = 42;
const TIFF_MAGIC_BIGTIFF: u16 = 43;
const TIFF_MAGIC_RW2: u16 = 0x55;
const BIGTIFF_OFFSET_SIZE: u16 = 8;
const X3F_SIGNATURE: &[u8; 4] = b"FOVb";
const X3F_IMAGE_DIRECTORY_ID: &[u8; 4] = b"IMA2";
const X3F_DIRECTORY_MIN_VERSION: u32 = 0x0002_0000;
const X3F_IMAGE_HEADER_BYTES: usize = 28;

const TAG_COMPRESSION: u16 = 0x0103;
const TAG_STRIP_OFFSETS: u16 = 0x0111;
const TAG_STRIP_BYTE_COUNTS: u16 = 0x0117;
const TAG_JPEG_INTERCHANGE_FORMAT: u16 = 0x0201;
const TAG_JPEG_INTERCHANGE_FORMAT_LENGTH: u16 = 0x0202;
const TAG_PANASONIC_JPG_FROM_RAW: u16 = 0x002E;

const TIFF_TYPE_SHORT: u16 = 3;
const TIFF_TYPE_LONG: u16 = 4;
const TIFF_TYPE_UNDEFINED: u16 = 7;
const TIFF_TYPE_LONG8: u16 = 16;
const COMPRESSION_JPEG_NEW: u16 = 7;

const BOX_TYPE_MOOV: [u8; 4] = *b"moov";
const BOX_TYPE_TRAK: [u8; 4] = *b"trak";
const BOX_TYPE_MDIA: [u8; 4] = *b"mdia";
const BOX_TYPE_MINF: [u8; 4] = *b"minf";
const BOX_TYPE_STBL: [u8; 4] = *b"stbl";
const BOX_TYPE_STSC: [u8; 4] = *b"stsc";
const BOX_TYPE_STCO: [u8; 4] = *b"stco";
const BOX_TYPE_STSZ: [u8; 4] = *b"stsz";

#[test]
fn raw_formats_extract_expected_embedded_preview_bytes() {
    let temp = tempdir().expect("tempdir");
    let hint = thumbnail_hint();
    type RawBuilder = fn(&[u8]) -> Vec<u8>;

    let cases: Vec<(&str, Vec<u8>, RawBuilder)> = vec![
        (
            "arw",
            sample_jpeg_bytes(640, 480),
            build_tiff_embedded_jpeg as RawBuilder,
        ),
        (
            "cr2",
            sample_jpeg_bytes(672, 504),
            build_tiff_embedded_jpeg as RawBuilder,
        ),
        (
            "cr3",
            sample_jpeg_bytes(704, 528),
            build_cr3_preview as RawBuilder,
        ),
        (
            "dng",
            sample_jpeg_bytes(736, 552),
            build_bigtiff_embedded_jpeg as RawBuilder,
        ),
        (
            "nef",
            sample_jpeg_bytes(768, 576),
            build_tiff_strip_preview as RawBuilder,
        ),
        (
            "orf",
            sample_jpeg_bytes(800, 600),
            build_tiff_strip_preview as RawBuilder,
        ),
        (
            "raf",
            sample_jpeg_bytes(832, 624),
            build_raf_preview as RawBuilder,
        ),
        (
            "rw2",
            sample_jpeg_bytes(864, 648),
            build_rw2_preview as RawBuilder,
        ),
        (
            "x3f",
            sample_jpeg_bytes(896, 672),
            build_x3f_preview as RawBuilder,
        ),
    ];

    let references = cases
        .iter()
        .map(|(extension, jpeg, _)| (*extension, downscaled_reference(jpeg)))
        .collect::<Vec<_>>();
    for (extension, jpeg, builder) in &cases {
        let extension = *extension;
        let path = temp.path().join(format!("sample.{extension}"));
        std::fs::write(&path, builder(jpeg)).expect("write synthetic raw");

        let result = ThumbnailPipeline::generate_from_path(&path, &hint, Some("image/jpeg"))
            .unwrap_or_else(|| panic!("expected embedded preview for .{extension}"));
        // Byte hashes are platform specific: jpeg-decoder's SIMD IDCT differs between x86_64 and
        // aarch64. Compare pixels instead, against the embedded preview itself and against the
        // previews of the other formats, so extracting the wrong image still fails.
        let actual = decode_rgb(&result.data);
        let mut own_error = f64::INFINITY;
        let mut closest_other_error = f64::INFINITY;
        for (reference_extension, reference) in &references {
            let error = mean_abs_error(&actual, reference);
            if *reference_extension == extension {
                own_error = error;
            } else {
                closest_other_error = closest_other_error.min(error);
            }
        }
        // The high-frequency test pattern aliases differently under DCT scaling and box
        // filtering (about 24 levels); a different preview is about 72 levels away.
        assert!(
            own_error < 40.0 && own_error * 2.0 < closest_other_error,
            ".{extension} thumbnail is not its embedded preview: own error {own_error:.1}, \
             closest other preview {closest_other_error:.1}"
        );
        assert_eq!((result.width, result.height), (256, 192));
        assert_eq!(
            result.format,
            iris_core_domain::types::thumbnail::ImageFormat::Jpeg,
            "final thumbnail format changed for .{extension}"
        );
        assert_eq!(
            result.width.max(result.height),
            hint.max_dimension,
            "expected normalized 256px output for .{extension}"
        );
    }
}

fn decode_rgb(jpeg: &[u8]) -> RgbImage {
    image::load_from_memory_with_format(jpeg, ImageFormat::Jpeg)
        .expect("decode jpeg")
        .to_rgb8()
}

fn downscaled_reference(jpeg: &[u8]) -> RgbImage {
    image::imageops::thumbnail(&decode_rgb(jpeg), 256, 192)
}

fn mean_abs_error(left: &RgbImage, right: &RgbImage) -> f64 {
    assert_eq!(left.dimensions(), right.dimensions());
    let total = left
        .as_raw()
        .iter()
        .zip(right.as_raw())
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum::<u64>();
    total as f64 / left.as_raw().len() as f64
}

#[test]
fn raw_formats_without_structured_preview_return_unsupported() {
    let temp = tempdir().expect("tempdir");
    let hint = thumbnail_hint();
    let disguised_jpeg = sample_jpeg_bytes(32, 24);
    let cancel_token = CancellationToken::new();

    for extension in [
        "arw", "cr2", "cr3", "dng", "nef", "orf", "raf", "rw2", "x3f",
    ] {
        let path = temp.path().join(format!("missing-preview.{extension}"));
        std::fs::write(&path, &disguised_jpeg).expect("write disguised raw");

        let result = ThumbnailPipeline::generate_from_path_result_with_cancellation(
            &path,
            &hint,
            Some("image/jpeg"),
            &cancel_token,
        );

        assert!(
            matches!(
                result,
                Err(BackendError::Unsupported {
                    operation: "raw embedded preview missing",
                })
            ),
            "expected unsupported fallback for .{extension}, got {result:?}"
        );
    }
}

fn thumbnail_hint() -> ThumbnailHint {
    ThumbnailHint {
        max_dimension: 256,
        preferred_formats: vec![
            iris_core_domain::types::thumbnail::ImageFormat::Jpeg,
            iris_core_domain::types::thumbnail::ImageFormat::Png,
        ],
    }
}

fn sample_jpeg_bytes(width: u32, height: u32) -> Vec<u8> {
    let image = RgbImage::from_fn(width, height, |x, y| {
        Rgb([
            ((x * 17 + y * 3) % 256) as u8,
            ((x * 5 + y * 11) % 256) as u8,
            ((x * 13 + y * 7) % 256) as u8,
        ])
    });
    let mut output = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(image)
        .write_to(&mut output, ImageFormat::Jpeg)
        .expect("encode jpeg");
    output.into_inner()
}

fn build_tiff_embedded_jpeg(jpeg: &[u8]) -> Vec<u8> {
    let jpeg_offset = 64u32;
    let jpeg_len = jpeg.len() as u32;

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&TIFF_MAGIC_STANDARD.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&2u16.to_le_bytes());

    tiff.extend_from_slice(&TAG_JPEG_INTERCHANGE_FORMAT.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&jpeg_offset.to_le_bytes());

    tiff.extend_from_slice(&TAG_JPEG_INTERCHANGE_FORMAT_LENGTH.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&jpeg_len.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());

    if tiff.len() < jpeg_offset as usize {
        tiff.resize(jpeg_offset as usize, 0);
    }
    tiff.extend_from_slice(jpeg);
    tiff
}

fn build_bigtiff_embedded_jpeg(jpeg: &[u8]) -> Vec<u8> {
    let jpeg_offset = 96u64;

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&TIFF_MAGIC_BIGTIFF.to_le_bytes());
    tiff.extend_from_slice(&BIGTIFF_OFFSET_SIZE.to_le_bytes());
    tiff.extend_from_slice(&0u16.to_le_bytes());
    tiff.extend_from_slice(&16u64.to_le_bytes());
    tiff.extend_from_slice(&2u64.to_le_bytes());

    tiff.extend_from_slice(&TAG_JPEG_INTERCHANGE_FORMAT.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG8.to_le_bytes());
    tiff.extend_from_slice(&1u64.to_le_bytes());
    tiff.extend_from_slice(&jpeg_offset.to_le_bytes());

    tiff.extend_from_slice(&TAG_JPEG_INTERCHANGE_FORMAT_LENGTH.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG8.to_le_bytes());
    tiff.extend_from_slice(&1u64.to_le_bytes());
    tiff.extend_from_slice(&(jpeg.len() as u64).to_le_bytes());
    tiff.extend_from_slice(&0u64.to_le_bytes());

    if tiff.len() < jpeg_offset as usize {
        tiff.resize(jpeg_offset as usize, 0);
    }
    tiff.extend_from_slice(jpeg);
    tiff
}

fn build_tiff_strip_preview(jpeg: &[u8]) -> Vec<u8> {
    let split_at = jpeg.len() / 2;
    let first = &jpeg[..split_at];
    let second = &jpeg[split_at..];
    let offsets_array_offset = 64u32;
    let counts_array_offset = offsets_array_offset + 8;
    let first_offset = 96u32;
    let second_offset = first_offset + first.len() as u32;

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&TIFF_MAGIC_STANDARD.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&3u16.to_le_bytes());

    tiff.extend_from_slice(&TAG_COMPRESSION.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_SHORT.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&(COMPRESSION_JPEG_NEW as u32).to_le_bytes());

    tiff.extend_from_slice(&TAG_STRIP_OFFSETS.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG.to_le_bytes());
    tiff.extend_from_slice(&2u32.to_le_bytes());
    tiff.extend_from_slice(&offsets_array_offset.to_le_bytes());

    tiff.extend_from_slice(&TAG_STRIP_BYTE_COUNTS.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG.to_le_bytes());
    tiff.extend_from_slice(&2u32.to_le_bytes());
    tiff.extend_from_slice(&counts_array_offset.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());

    if tiff.len() < offsets_array_offset as usize {
        tiff.resize(offsets_array_offset as usize, 0);
    }
    tiff.extend_from_slice(&first_offset.to_le_bytes());
    tiff.extend_from_slice(&second_offset.to_le_bytes());

    if tiff.len() < counts_array_offset as usize {
        tiff.resize(counts_array_offset as usize, 0);
    }
    tiff.extend_from_slice(&(first.len() as u32).to_le_bytes());
    tiff.extend_from_slice(&(second.len() as u32).to_le_bytes());

    if tiff.len() < first_offset as usize {
        tiff.resize(first_offset as usize, 0);
    }
    tiff.extend_from_slice(first);
    tiff.extend_from_slice(second);
    tiff
}

fn build_cr3_preview(jpeg: &[u8]) -> Vec<u8> {
    let invalid_sample = vec![0u8; 320];

    let stsc = bmff_box(
        BOX_TYPE_STSC,
        [
            0u32.to_be_bytes().to_vec(),
            1u32.to_be_bytes().to_vec(),
            1u32.to_be_bytes().to_vec(),
            2u32.to_be_bytes().to_vec(),
            1u32.to_be_bytes().to_vec(),
        ]
        .concat(),
    );
    let stsz = bmff_box(
        BOX_TYPE_STSZ,
        [
            0u32.to_be_bytes().to_vec(),
            0u32.to_be_bytes().to_vec(),
            2u32.to_be_bytes().to_vec(),
            (invalid_sample.len() as u32).to_be_bytes().to_vec(),
            (jpeg.len() as u32).to_be_bytes().to_vec(),
        ]
        .concat(),
    );
    let stco = |chunk_offset: u32| {
        bmff_box(
            BOX_TYPE_STCO,
            [
                0u32.to_be_bytes().to_vec(),
                1u32.to_be_bytes().to_vec(),
                chunk_offset.to_be_bytes().to_vec(),
            ]
            .concat(),
        )
    };
    let build_moov = |stco_box: Vec<u8>| {
        bmff_box(
            BOX_TYPE_MOOV,
            bmff_box(
                BOX_TYPE_TRAK,
                bmff_box(
                    BOX_TYPE_MDIA,
                    bmff_box(
                        BOX_TYPE_MINF,
                        bmff_box(
                            BOX_TYPE_STBL,
                            [stsc.clone(), stsz.clone(), stco_box].concat(),
                        ),
                    ),
                ),
            ),
        )
    };

    let moov = build_moov(stco(0));
    let moov = build_moov(stco(moov.len() as u32));
    [moov, invalid_sample, jpeg.to_vec()].concat()
}

fn bmff_box(typ: [u8; 4], payload: Vec<u8>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + payload.len());
    bytes.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
    bytes.extend_from_slice(&typ);
    bytes.extend_from_slice(&payload);
    bytes
}

fn build_rw2_preview(jpeg: &[u8]) -> Vec<u8> {
    let jpeg_offset = 80u32;

    let mut rw2 = Vec::new();
    rw2.extend_from_slice(b"II");
    rw2.extend_from_slice(&TIFF_MAGIC_RW2.to_le_bytes());
    rw2.extend_from_slice(&8u32.to_le_bytes());
    rw2.extend_from_slice(&1u16.to_le_bytes());
    rw2.extend_from_slice(&TAG_PANASONIC_JPG_FROM_RAW.to_le_bytes());
    rw2.extend_from_slice(&TIFF_TYPE_UNDEFINED.to_le_bytes());
    rw2.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
    rw2.extend_from_slice(&jpeg_offset.to_le_bytes());
    rw2.extend_from_slice(&0u32.to_le_bytes());

    if rw2.len() < jpeg_offset as usize {
        rw2.resize(jpeg_offset as usize, 0);
    }
    rw2.extend_from_slice(jpeg);
    rw2
}

fn build_raf_preview(jpeg: &[u8]) -> Vec<u8> {
    let jpeg_offset = 92u32;

    let mut raf = vec![0u8; jpeg_offset as usize];
    raf[84..88].copy_from_slice(&jpeg_offset.to_be_bytes());
    raf[88..92].copy_from_slice(&(jpeg.len() as u32).to_be_bytes());
    raf.extend_from_slice(jpeg);
    raf
}

fn build_x3f_preview(jpeg: &[u8]) -> Vec<u8> {
    let image_offset = 128u32;
    let image_len = (X3F_IMAGE_HEADER_BYTES + jpeg.len()) as u32;

    let mut x3f = vec![0u8; image_offset as usize];
    x3f[0..4].copy_from_slice(X3F_SIGNATURE);
    x3f.extend_from_slice(&0u32.to_le_bytes());
    x3f.extend_from_slice(&0u32.to_le_bytes());
    x3f.extend_from_slice(&0u32.to_le_bytes());
    x3f.extend_from_slice(&0u32.to_le_bytes());
    x3f.extend_from_slice(&(jpeg_width(jpeg) as u32).to_le_bytes());
    x3f.extend_from_slice(&(jpeg_height(jpeg) as u32).to_le_bytes());
    x3f.extend_from_slice(&0u32.to_le_bytes());
    x3f.extend_from_slice(jpeg);

    let directory_offset = x3f.len() as u32;
    x3f.extend_from_slice(&0u32.to_le_bytes());
    x3f.extend_from_slice(&X3F_DIRECTORY_MIN_VERSION.to_le_bytes());
    x3f.extend_from_slice(&1u32.to_le_bytes());
    x3f.extend_from_slice(&image_offset.to_le_bytes());
    x3f.extend_from_slice(&image_len.to_le_bytes());
    x3f.extend_from_slice(X3F_IMAGE_DIRECTORY_ID);
    x3f.extend_from_slice(&directory_offset.to_le_bytes());
    x3f
}

fn jpeg_width(jpeg: &[u8]) -> u16 {
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(jpeg));
    decoder.read_info().expect("read jpeg info");
    decoder.info().expect("jpeg info").width
}

fn jpeg_height(jpeg: &[u8]) -> u16 {
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(jpeg));
    decoder.read_info().expect("read jpeg info");
    decoder.info().expect("jpeg info").height
}
