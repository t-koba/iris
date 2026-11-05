use super::*;
use crate::test_fixtures::{
    bigtiff_with_jpeg, cr3_with_jpeg, jpeg, temp_path, tiff, tiff_with_inline_strips,
    tiff_with_jpeg,
};

/// Writes `bytes` to a temporary `.extension` file and extracts its thumbnail.
fn thumbnail_of(extension: &str, bytes: &[u8], max_dimension: u32) -> Option<ThumbnailResult> {
    let path = temp_path(extension);
    std::fs::write(&path, bytes).expect("write fixture");
    let result = extract_raw_thumbnail(&path, max_dimension);
    let _ = std::fs::remove_file(path);
    result
}

fn size(result: Option<ThumbnailResult>) -> (u32, u32) {
    let result = result.expect("embedded preview");
    (result.width, result.height)
}

#[test]
fn extracts_embedded_jpeg_from_structured_tiff_ifd() {
    let tiff = tiff_with_jpeg(64, &jpeg(48, 32));
    assert_eq!(size(thumbnail_of("dng", &tiff, 256)), (48, 32));
}

#[test]
fn extracts_inline_short_strip_offsets_and_counts() {
    let jpeg = jpeg(72, 48);
    let tiff = tiff_with_inline_strips(64, 64 + (jpeg.len() / 2) as u16, &jpeg);
    assert_eq!(size(thumbnail_of("nef", &tiff, 256)), (72, 48));
}

#[test]
fn extracts_embedded_jpeg_from_bigtiff_ifd() {
    let bigtiff = bigtiff_with_jpeg(96, &jpeg(64, 42));
    assert_eq!(size(thumbnail_of("dng", &bigtiff, 256)), (64, 42));
}

#[test]
fn extracts_noncontiguous_jpeg_strips_from_structured_offsets() {
    let tiff = tiff_with_inline_strips(96, 512, &jpeg(88, 58));
    assert_eq!(size(thumbnail_of("dng", &tiff, 256)), (88, 58));
}

#[test]
fn extracts_cr3_preview_from_later_sample_in_chunk() {
    let cr3 = cr3_with_jpeg(&jpeg(91, 61), false);
    assert_eq!(size(thumbnail_of("cr3", &cr3, 256)), (91, 61));
}

#[test]
fn extracts_cr3_preview_from_stz2_sample_sizes() {
    let cr3 = cr3_with_jpeg(&jpeg(93, 63), true);
    assert_eq!(size(thumbnail_of("cr3", &cr3, 256)), (93, 63));
}

#[cfg(feature = "raw-full")]
#[test]
fn extracts_rw2_jpg_from_raw_tag_with_nonstandard_magic() {
    let rw2 = crate::test_fixtures::rw2_with_jpeg(&jpeg(192, 128));
    assert_eq!(size(thumbnail_of("rw2", &rw2, 256)), (192, 128));
}

#[test]
fn prefers_smallest_sufficient_structured_candidate() {
    let small = jpeg(240, 160);
    let large = jpeg(640, 480);
    let small_offset = 80u32;
    let large_offset = small_offset + small.len() as u32;
    let entry = |tag: u16, len: usize, offset: u32| {
        [
            tag.to_le_bytes().as_slice(),
            &TIFF_TYPE_UNDEFINED.to_le_bytes(),
            &(len as u32).to_le_bytes(),
            &offset.to_le_bytes(),
        ]
        .concat()
    };
    let raw = tiff(
        TIFF_MAGIC_RW2,
        &[
            entry(TAG_PANASONIC_JPG_FROM_RAW, small.len(), small_offset),
            entry(TAG_PANASONIC_JPG_FROM_RAW2, large.len(), large_offset),
        ],
        small_offset as usize,
        &[small, large].concat(),
    );
    assert_eq!(size(thumbnail_of("dng", &raw, 200)), (200, 133));
}

#[cfg(feature = "raw-full")]
#[test]
fn extracts_raf_embedded_jpeg_from_header_offsets() {
    let raf = crate::test_fixtures::raf_with_jpeg(&jpeg(96, 64));
    assert_eq!(size(thumbnail_of("raf", &raf, 256)), (96, 64));
}

#[cfg(feature = "raw-full")]
#[test]
fn extracts_x3f_embedded_jpeg_from_directory_image() {
    let x3f = crate::test_fixtures::x3f_with_jpeg(&jpeg(80, 60));
    assert_eq!(size(thumbnail_of("x3f", &x3f, 256)), (80, 60));
}

#[cfg(feature = "raw-full")]
#[test]
fn x3f_without_structured_preview_does_not_marker_scan() {
    let mut x3f = X3F_SIGNATURE.to_vec();
    x3f.resize(128, 0);
    x3f.extend_from_slice(&jpeg(64, 64));
    let directory_offset = x3f.len() as u32;
    for value in [0, X3F_DIRECTORY_MIN_VERSION, 0, directory_offset] {
        x3f.extend_from_slice(&value.to_le_bytes());
    }
    assert!(thumbnail_of("x3f", &x3f, 256).is_none());
}
