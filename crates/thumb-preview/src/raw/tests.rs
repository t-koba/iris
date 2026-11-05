use super::*;
use std::io::Cursor;

fn unique_temp_raw_path(ext: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("iris-thumb-preview-test-{}.{}", nanos, ext))
}

fn encoded_test_jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut jpeg = Vec::new();
    image::DynamicImage::new_rgb8(width, height)
        .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .expect("jpeg encode");
    jpeg
}

fn inline_short_pair_le(first: u16, second: u16) -> u32 {
    u32::from_le_bytes([
        first.to_le_bytes()[0],
        first.to_le_bytes()[1],
        second.to_le_bytes()[0],
        second.to_le_bytes()[1],
    ])
}

fn split_jpeg_strips_tiff(first_offset: u16, second_offset: u16, jpeg: &[u8]) -> Vec<u8> {
    let split_at = jpeg.len() / 2;
    let first = &jpeg[..split_at];
    let second = &jpeg[split_at..];

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
    tiff.extend_from_slice(&TIFF_TYPE_SHORT.to_le_bytes());
    tiff.extend_from_slice(&2u32.to_le_bytes());
    tiff.extend_from_slice(&inline_short_pair_le(first_offset, second_offset).to_le_bytes());

    tiff.extend_from_slice(&TAG_STRIP_BYTE_COUNTS.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_SHORT.to_le_bytes());
    tiff.extend_from_slice(&2u32.to_le_bytes());
    tiff.extend_from_slice(
        &inline_short_pair_le(first.len() as u16, second.len() as u16).to_le_bytes(),
    );

    tiff.extend_from_slice(&0u32.to_le_bytes());

    if tiff.len() < first_offset as usize {
        tiff.resize(first_offset as usize, 0);
    }
    tiff.extend_from_slice(first);
    if tiff.len() < second_offset as usize {
        tiff.resize(second_offset as usize, 0);
    }
    tiff.extend_from_slice(second);
    tiff
}

fn bigtiff_with_embedded_jpeg(jpeg_offset: u64, jpeg: &[u8]) -> Vec<u8> {
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

fn bmff_box(typ: [u8; 4], payload: Vec<u8>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + payload.len());
    bytes.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
    bytes.extend_from_slice(&typ);
    bytes.extend_from_slice(&payload);
    bytes
}

fn synthetic_cr3_with_second_sample_preview(jpeg: &[u8]) -> Vec<u8> {
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

    let stco_with_offset = |chunk_offset: u32| {
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

    let build_moov = |stco: Vec<u8>| {
        bmff_box(
            BOX_TYPE_MOOV,
            bmff_box(
                BOX_TYPE_TRAK,
                bmff_box(
                    BOX_TYPE_MDIA,
                    bmff_box(
                        BOX_TYPE_MINF,
                        bmff_box(BOX_TYPE_STBL, [stsc.clone(), stsz.clone(), stco].concat()),
                    ),
                ),
            ),
        )
    };

    let moov = build_moov(stco_with_offset(0));
    let chunk_offset = moov.len() as u32;
    let moov = build_moov(stco_with_offset(chunk_offset));

    [moov, invalid_sample, jpeg.to_vec()].concat()
}

fn synthetic_cr3_with_stz2_second_sample_preview(jpeg: &[u8]) -> Vec<u8> {
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
    let stz2 = bmff_box(
        BOX_TYPE_STZ2,
        [
            0u32.to_be_bytes().to_vec(),
            vec![0, 0, 0, 16],
            2u32.to_be_bytes().to_vec(),
            (invalid_sample.len() as u16).to_be_bytes().to_vec(),
            (jpeg.len() as u16).to_be_bytes().to_vec(),
        ]
        .concat(),
    );

    let stco_with_offset = |chunk_offset: u32| {
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

    let build_moov = |stco: Vec<u8>| {
        bmff_box(
            BOX_TYPE_MOOV,
            bmff_box(
                BOX_TYPE_TRAK,
                bmff_box(
                    BOX_TYPE_MDIA,
                    bmff_box(
                        BOX_TYPE_MINF,
                        bmff_box(BOX_TYPE_STBL, [stsc.clone(), stz2.clone(), stco].concat()),
                    ),
                ),
            ),
        )
    };

    let moov = build_moov(stco_with_offset(0));
    let chunk_offset = moov.len() as u32;
    let moov = build_moov(stco_with_offset(chunk_offset));

    [moov, invalid_sample, jpeg.to_vec()].concat()
}

#[test]
fn extracts_embedded_jpeg_from_structured_tiff_ifd() {
    let jpeg = encoded_test_jpeg(48, 32);
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
    tiff.extend_from_slice(&jpeg);

    let path = unique_temp_raw_path("dng");
    std::fs::write(&path, &tiff).expect("write dng-like file");

    let result = extract_raw_thumbnail(&path, 256).expect("structured embedded jpeg");
    assert_eq!((result.width, result.height), (48, 32));

    let _ = std::fs::remove_file(path);
}

#[test]
fn extracts_inline_short_strip_offsets_and_counts() {
    let jpeg = encoded_test_jpeg(72, 48);
    let tiff = split_jpeg_strips_tiff(64, 64 + (jpeg.len() / 2) as u16, &jpeg);

    let path = unique_temp_raw_path("nef");
    std::fs::write(&path, &tiff).expect("write strip-based tiff");

    let result = extract_raw_thumbnail(&path, 256).expect("inline short strip preview");
    assert_eq!((result.width, result.height), (72, 48));

    let _ = std::fs::remove_file(path);
}

#[test]
fn extracts_embedded_jpeg_from_bigtiff_ifd() {
    let jpeg = encoded_test_jpeg(64, 42);
    let bigtiff = bigtiff_with_embedded_jpeg(96, &jpeg);

    let path = unique_temp_raw_path("dng");
    std::fs::write(&path, &bigtiff).expect("write bigtiff-like dng");

    let result = extract_raw_thumbnail(&path, 256).expect("bigtiff embedded jpeg");
    assert_eq!((result.width, result.height), (64, 42));

    let _ = std::fs::remove_file(path);
}

#[test]
fn extracts_noncontiguous_jpeg_strips_from_structured_offsets() {
    let jpeg = encoded_test_jpeg(88, 58);
    let tiff = split_jpeg_strips_tiff(96, 512, &jpeg);

    let path = unique_temp_raw_path("orf");
    std::fs::write(&path, &tiff).expect("write fragmented strip tiff");

    let result = extract_raw_thumbnail(&path, 256).expect("fragmented strip preview");
    assert_eq!((result.width, result.height), (88, 58));

    let _ = std::fs::remove_file(path);
}

#[test]
fn extracts_cr3_preview_from_later_sample_in_chunk() {
    let jpeg = encoded_test_jpeg(91, 61);
    let cr3 = synthetic_cr3_with_second_sample_preview(&jpeg);

    let path = unique_temp_raw_path("cr3");
    std::fs::write(&path, &cr3).expect("write synthetic cr3");

    let result = extract_raw_thumbnail(&path, 256).expect("cr3 later-sample preview");
    assert_eq!((result.width, result.height), (91, 61));

    let _ = std::fs::remove_file(path);
}

#[test]
fn extracts_cr3_preview_from_stz2_sample_sizes() {
    let jpeg = encoded_test_jpeg(93, 63);
    let cr3 = synthetic_cr3_with_stz2_second_sample_preview(&jpeg);

    let path = unique_temp_raw_path("cr3");
    std::fs::write(&path, &cr3).expect("write synthetic cr3 stz2");

    let result = extract_raw_thumbnail(&path, 256).expect("cr3 stz2 preview");
    assert_eq!((result.width, result.height), (93, 63));

    let _ = std::fs::remove_file(path);
}

#[test]
fn extracts_rw2_jpg_from_raw_tag_with_nonstandard_magic() {
    let jpeg = encoded_test_jpeg(192, 128);
    let jpeg_offset = 64u32;

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
    rw2.extend_from_slice(&jpeg);

    let path = unique_temp_raw_path("rw2");
    std::fs::write(&path, &rw2).expect("write rw2-like file");

    let result = extract_raw_thumbnail(&path, 256).expect("rw2 preview");
    assert_eq!((result.width, result.height), (192, 128));

    let _ = std::fs::remove_file(path);
}

#[test]
fn prefers_smallest_sufficient_structured_candidate() {
    let small_jpeg = encoded_test_jpeg(240, 160);
    let large_jpeg = encoded_test_jpeg(640, 480);
    let small_offset = 80u32;
    let large_offset = small_offset + small_jpeg.len() as u32;

    let mut rw2 = Vec::new();
    rw2.extend_from_slice(b"II");
    rw2.extend_from_slice(&TIFF_MAGIC_RW2.to_le_bytes());
    rw2.extend_from_slice(&8u32.to_le_bytes());

    rw2.extend_from_slice(&2u16.to_le_bytes());
    rw2.extend_from_slice(&TAG_PANASONIC_JPG_FROM_RAW.to_le_bytes());
    rw2.extend_from_slice(&TIFF_TYPE_UNDEFINED.to_le_bytes());
    rw2.extend_from_slice(&(small_jpeg.len() as u32).to_le_bytes());
    rw2.extend_from_slice(&small_offset.to_le_bytes());
    rw2.extend_from_slice(&TAG_PANASONIC_JPG_FROM_RAW2.to_le_bytes());
    rw2.extend_from_slice(&TIFF_TYPE_UNDEFINED.to_le_bytes());
    rw2.extend_from_slice(&(large_jpeg.len() as u32).to_le_bytes());
    rw2.extend_from_slice(&large_offset.to_le_bytes());
    rw2.extend_from_slice(&0u32.to_le_bytes());

    if rw2.len() < small_offset as usize {
        rw2.resize(small_offset as usize, 0);
    }
    rw2.extend_from_slice(&small_jpeg);
    rw2.extend_from_slice(&large_jpeg);

    let path = unique_temp_raw_path("rw2");
    std::fs::write(&path, &rw2).expect("write rw2-like file");

    let result = extract_raw_thumbnail(&path, 200).expect("best candidate");
    assert_eq!((result.width, result.height), (200, 133));

    let _ = std::fs::remove_file(path);
}

#[cfg(feature = "raw-full")]
#[test]
fn extracts_raf_embedded_jpeg_from_header_offsets() {
    let jpeg = encoded_test_jpeg(96, 64);
    let jpeg_offset = 92u32;

    let mut raf = vec![0u8; jpeg_offset as usize];
    raf[84..88].copy_from_slice(&jpeg_offset.to_be_bytes());
    raf[88..92].copy_from_slice(&(jpeg.len() as u32).to_be_bytes());
    raf.extend_from_slice(&jpeg);

    let path = unique_temp_raw_path("raf");
    std::fs::write(&path, &raf).expect("write raf-like file");

    let result = extract_raw_thumbnail(&path, 256).expect("raf preview");
    assert_eq!((result.width, result.height), (96, 64));

    let _ = std::fs::remove_file(path);
}

#[cfg(feature = "raw-full")]
#[test]
fn extracts_x3f_embedded_jpeg_from_directory_image() {
    let jpeg = encoded_test_jpeg(80, 60);
    let image_offset = 128u32;
    let image_len = (X3F_IMAGE_HEADER_BYTES + jpeg.len()) as u32;

    let mut x3f = vec![0u8; image_offset as usize];
    x3f[0..4].copy_from_slice(X3F_SIGNATURE);
    x3f.extend_from_slice(&0u32.to_le_bytes()); // reserved
    x3f.extend_from_slice(&0u32.to_le_bytes()); // reserved
    x3f.extend_from_slice(&0u32.to_le_bytes()); // typ
    x3f.extend_from_slice(&0u32.to_le_bytes()); // format
    x3f.extend_from_slice(&80u32.to_le_bytes());
    x3f.extend_from_slice(&60u32.to_le_bytes());
    x3f.extend_from_slice(&0u32.to_le_bytes()); // pitch
    x3f.extend_from_slice(&jpeg);

    let directory_offset = x3f.len() as u32;
    x3f.extend_from_slice(&0u32.to_le_bytes());
    x3f.extend_from_slice(&X3F_DIRECTORY_MIN_VERSION.to_le_bytes());
    x3f.extend_from_slice(&1u32.to_le_bytes());
    x3f.extend_from_slice(&image_offset.to_le_bytes());
    x3f.extend_from_slice(&image_len.to_le_bytes());
    x3f.extend_from_slice(X3F_IMAGE_DIRECTORY_ID);
    x3f.extend_from_slice(&directory_offset.to_le_bytes());

    let path = unique_temp_raw_path("x3f");
    std::fs::write(&path, &x3f).expect("write x3f-like file");

    let result = extract_raw_thumbnail(&path, 256).expect("x3f preview");
    assert_eq!((result.width, result.height), (80, 60));

    let _ = std::fs::remove_file(path);
}

#[cfg(feature = "raw-full")]
#[test]
fn x3f_without_structured_preview_does_not_marker_scan() {
    let jpeg = encoded_test_jpeg(64, 64);
    let mut x3f = Vec::new();
    x3f.extend_from_slice(X3F_SIGNATURE);
    x3f.resize(128, 0);
    x3f.extend_from_slice(&jpeg);
    let directory_offset = x3f.len() as u32;
    x3f.extend_from_slice(&0u32.to_le_bytes());
    x3f.extend_from_slice(&X3F_DIRECTORY_MIN_VERSION.to_le_bytes());
    x3f.extend_from_slice(&0u32.to_le_bytes());
    x3f.extend_from_slice(&directory_offset.to_le_bytes());

    let path = unique_temp_raw_path("x3f");
    std::fs::write(&path, &x3f).expect("write x3f file without structured preview");

    assert!(extract_raw_thumbnail(&path, 256).is_none());

    let _ = std::fs::remove_file(path);
}
