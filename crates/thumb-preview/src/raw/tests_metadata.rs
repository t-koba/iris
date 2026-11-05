use super::*;

fn unique_temp_raw_path(ext: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("iris-thumb-preview-test-{}.{}", nanos, ext))
}

fn tiff_with_primary_and_preview_ifds() -> Vec<u8> {
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&TIFF_MAGIC_RW2.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes());

    let preview_ifd_offset = 8 + 2 + (2 * 12) + 4;

    tiff.extend_from_slice(&2u16.to_le_bytes());
    tiff.extend_from_slice(&TAG_IMAGE_WIDTH.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&8368u32.to_le_bytes());
    tiff.extend_from_slice(&TAG_IMAGE_LENGTH.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&5584u32.to_le_bytes());
    tiff.extend_from_slice(&(preview_ifd_offset as u32).to_le_bytes());

    tiff.extend_from_slice(&3u16.to_le_bytes());
    tiff.extend_from_slice(&TAG_NEW_SUBFILE_TYPE.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&TAG_IMAGE_WIDTH.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&1920u32.to_le_bytes());
    tiff.extend_from_slice(&TAG_IMAGE_LENGTH.to_le_bytes());
    tiff.extend_from_slice(&TIFF_TYPE_LONG.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&1280u32.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());

    tiff
}

#[cfg(feature = "raw-full")]
fn rw2_with_sensor_borders() -> Vec<u8> {
    let mut rw2 = Vec::new();
    rw2.extend_from_slice(b"II");
    rw2.extend_from_slice(&TIFF_MAGIC_RW2.to_le_bytes());
    rw2.extend_from_slice(&8u32.to_le_bytes());

    rw2.extend_from_slice(&6u16.to_le_bytes());

    for (tag, value) in [
        (TAG_PANASONIC_SENSOR_WIDTH, 8152u32),
        (TAG_PANASONIC_SENSOR_HEIGHT, 5432u32),
        (TAG_PANASONIC_SENSOR_TOP_BORDER, 4u32),
        (TAG_PANASONIC_SENSOR_LEFT_BORDER, 4u32),
        (TAG_PANASONIC_SENSOR_BOTTOM_BORDER, 5428u32),
        (TAG_PANASONIC_SENSOR_RIGHT_BORDER, 8148u32),
    ] {
        rw2.extend_from_slice(&tag.to_le_bytes());
        rw2.extend_from_slice(&TIFF_TYPE_SHORT.to_le_bytes());
        rw2.extend_from_slice(&1u32.to_le_bytes());
        rw2.extend_from_slice(&value.to_le_bytes());
    }

    rw2.extend_from_slice(&0u32.to_le_bytes());
    rw2
}

#[cfg(feature = "raw-full")]
fn rw2_with_structured_metadata_entries() -> Vec<u8> {
    let mut rw2 = Vec::new();
    rw2.extend_from_slice(b"II");
    rw2.extend_from_slice(&TIFF_MAGIC_RW2.to_le_bytes());
    rw2.extend_from_slice(&8u32.to_le_bytes());

    rw2.extend_from_slice(&9u16.to_le_bytes());

    for (tag, field_type, value) in [
        (TAG_PANASONIC_SENSOR_WIDTH, TIFF_TYPE_SHORT, 8152u32),
        (TAG_PANASONIC_SENSOR_HEIGHT, TIFF_TYPE_SHORT, 5432u32),
        (TAG_PANASONIC_SENSOR_TOP_BORDER, TIFF_TYPE_SHORT, 4u32),
        (TAG_PANASONIC_SENSOR_LEFT_BORDER, TIFF_TYPE_SHORT, 4u32),
        (TAG_PANASONIC_SENSOR_BOTTOM_BORDER, TIFF_TYPE_SHORT, 5428u32),
        (TAG_PANASONIC_SENSOR_RIGHT_BORDER, TIFF_TYPE_SHORT, 8148u32),
        (TAG_PANASONIC_BITS_PER_SAMPLE, TIFF_TYPE_SHORT, 14u32),
        (TAG_PANASONIC_RAW_FORMAT, TIFF_TYPE_SHORT, 8u32),
    ] {
        rw2.extend_from_slice(&tag.to_le_bytes());
        rw2.extend_from_slice(&field_type.to_le_bytes());
        rw2.extend_from_slice(&1u32.to_le_bytes());
        rw2.extend_from_slice(&value.to_le_bytes());
    }

    rw2.extend_from_slice(&TAG_PANASONIC_RAW_VERSION.to_le_bytes());
    rw2.extend_from_slice(&TIFF_TYPE_UNDEFINED.to_le_bytes());
    rw2.extend_from_slice(&4u32.to_le_bytes());
    rw2.extend_from_slice(b"0500");
    rw2.extend_from_slice(&0u32.to_le_bytes());
    rw2
}

#[cfg(feature = "raw-full")]
fn x3f_with_header4_metadata(width: u32, height: u32, version: u32, rotation: u32) -> Vec<u8> {
    let mut x3f = vec![0u8; 768];
    x3f[0..4].copy_from_slice(X3F_SIGNATURE);
    x3f[4..8].copy_from_slice(&version.to_le_bytes());
    x3f[0x28..0x2c].copy_from_slice(&width.to_le_bytes());
    x3f[0x2c..0x30].copy_from_slice(&height.to_le_bytes());
    x3f[0x30..0x34].copy_from_slice(&rotation.to_le_bytes());
    x3f
}

#[test]
fn extracts_primary_dimensions_from_main_raw_ifd() {
    let raw = tiff_with_primary_and_preview_ifds();

    let path = unique_temp_raw_path("dng");
    std::fs::write(&path, &raw).expect("write metadata file");

    let metadata = extract_raw_metadata(&path).expect("raw metadata");
    assert_eq!(metadata.width, Some(8368));
    assert_eq!(metadata.height, Some(5584));

    let _ = std::fs::remove_file(path);
}

#[cfg(feature = "raw-full")]
#[test]
fn extracts_panasonic_rw2_dimensions_from_sensor_borders() {
    let raw = rw2_with_sensor_borders();

    let path = unique_temp_raw_path("rw2");
    std::fs::write(&path, &raw).expect("write rw2 sensor border metadata file");

    let metadata = extract_raw_metadata(&path).expect("rw2 metadata");
    assert_eq!(metadata.width, Some(8144));
    assert_eq!(metadata.height, Some(5424));

    let _ = std::fs::remove_file(path);
}

#[cfg(feature = "raw-full")]
#[test]
fn extracts_panasonic_rw2_container_entries_without_preview_fallback() {
    let raw = rw2_with_structured_metadata_entries();

    let path = unique_temp_raw_path("rw2");
    std::fs::write(&path, &raw).expect("write rw2 structured metadata file");

    let metadata = extract_raw_metadata(&path).expect("rw2 metadata");
    assert!(metadata
        .exif_entries
        .iter()
        .any(|(tag, value)| { tag == "Bits Per Sample" && value == "14" }));
    assert!(metadata
        .exif_entries
        .iter()
        .any(|(tag, value)| { tag == "Panasonic Raw Version" && value == "0500" }));
    assert!(metadata
        .exif_entries
        .iter()
        .any(|(tag, value)| { tag == "Raw Format" && value == "8" }));

    let _ = std::fs::remove_file(path);
}

#[cfg(feature = "raw-full")]
#[test]
fn extracts_x3f_metadata_from_header4() {
    let x3f = x3f_with_header4_metadata(5424, 3616, 0x0004_0001, 0);

    let path = unique_temp_raw_path("x3f");
    std::fs::write(&path, &x3f).expect("write x3f metadata file");

    let metadata = extract_raw_metadata(&path).expect("x3f metadata");
    assert_eq!(metadata.width, Some(5424));
    assert_eq!(metadata.height, Some(3616));
    assert!(metadata
        .exif_entries
        .iter()
        .any(|(tag, value)| { tag == "File Version" && value == "4.1" }));
    assert!(metadata
        .exif_entries
        .iter()
        .any(|(tag, value)| { tag == "Rotation" && value == "0" }));

    let _ = std::fs::remove_file(path);
}
