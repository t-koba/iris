use image::{ImageFormat, RgbImage};
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::thumbnail::ThumbnailHint;
use iris_thumb_preview::pipeline::ThumbnailPipeline;
use iris_thumb_preview::test_fixtures::{
    bigtiff_with_jpeg, cr3_with_jpeg, jpeg, raf_with_jpeg, rw2_with_jpeg, tiff_with_jpeg,
    tiff_with_strips, x3f_with_jpeg,
};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

#[test]
fn raw_formats_extract_expected_embedded_preview_bytes() {
    let temp = tempdir().expect("tempdir");
    let hint = thumbnail_hint();
    type RawBuilder = fn(&[u8]) -> Vec<u8>;

    let cases: Vec<(&str, Vec<u8>, RawBuilder)> = vec![
        (
            "arw",
            jpeg(640, 480),
            (|jpeg| tiff_with_jpeg(64, jpeg)) as RawBuilder,
        ),
        (
            "cr2",
            jpeg(672, 504),
            (|jpeg| tiff_with_jpeg(64, jpeg)) as RawBuilder,
        ),
        (
            "cr3",
            jpeg(704, 528),
            (|jpeg| cr3_with_jpeg(jpeg, false)) as RawBuilder,
        ),
        (
            "dng",
            jpeg(736, 552),
            (|jpeg| bigtiff_with_jpeg(96, jpeg)) as RawBuilder,
        ),
        ("nef", jpeg(768, 576), tiff_with_strips as RawBuilder),
        ("orf", jpeg(800, 600), tiff_with_strips as RawBuilder),
        ("raf", jpeg(832, 624), raf_with_jpeg as RawBuilder),
        ("rw2", jpeg(864, 648), rw2_with_jpeg as RawBuilder),
        ("x3f", jpeg(896, 672), x3f_with_jpeg as RawBuilder),
    ];

    let references = cases
        .iter()
        .map(|(extension, jpeg, _)| (*extension, downscaled_reference(jpeg)))
        .collect::<Vec<_>>();
    for (extension, jpeg, builder) in &cases {
        let extension = *extension;
        let path = temp.path().join(format!("sample.{extension}"));
        std::fs::write(&path, builder(jpeg)).expect("write synthetic raw");

        let result = ThumbnailPipeline::generate_from_path(
            &path,
            &hint,
            Some("image/jpeg"),
            &CancellationToken::new(),
        )
        .unwrap_or_else(|error| panic!("expected embedded preview for .{extension}: {error}"));
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
    let disguised_jpeg = jpeg(32, 24);
    let cancel_token = CancellationToken::new();

    for extension in [
        "arw", "cr2", "cr3", "dng", "nef", "orf", "raf", "rw2", "x3f",
    ] {
        let path = temp.path().join(format!("missing-preview.{extension}"));
        std::fs::write(&path, &disguised_jpeg).expect("write disguised raw");

        let result =
            ThumbnailPipeline::generate_from_path(&path, &hint, Some("image/jpeg"), &cancel_token);

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
