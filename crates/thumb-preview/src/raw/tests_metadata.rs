use super::*;
use crate::test_fixtures::{ifd_entry, temp_path, tiff};

/// Writes `bytes` to a temporary `.extension` file and extracts its metadata.
fn metadata_of(extension: &str, bytes: &[u8]) -> RawMetadata {
    let path = temp_path(extension);
    std::fs::write(&path, bytes).expect("write fixture");
    let metadata = extract_raw_metadata(&path);
    let _ = std::fs::remove_file(path);
    metadata.expect("raw metadata")
}

fn has_entry(metadata: &RawMetadata, tag: &str, value: &str) -> bool {
    metadata
        .exif_entries
        .iter()
        .any(|(entry_tag, entry_value)| entry_tag == tag && entry_value == value)
}

#[test]
fn extracts_primary_dimensions_from_main_raw_ifd() {
    // The main IFD is followed by a reduced-resolution preview IFD.
    let preview_ifd = 8 + 2 + 2 * 12 + 4;
    let mut raw = tiff(
        TIFF_MAGIC_RW2,
        &[
            ifd_entry(TAG_IMAGE_WIDTH, TIFF_TYPE_LONG, 1, 8368),
            ifd_entry(TAG_IMAGE_LENGTH, TIFF_TYPE_LONG, 1, 5584),
        ],
        0,
        &[],
    );
    raw.truncate(raw.len() - 4);
    raw.extend_from_slice(&(preview_ifd as u32).to_le_bytes());
    raw.extend_from_slice(&3u16.to_le_bytes());
    for entry in [
        ifd_entry(TAG_NEW_SUBFILE_TYPE, TIFF_TYPE_LONG, 1, 1),
        ifd_entry(TAG_IMAGE_WIDTH, TIFF_TYPE_LONG, 1, 1920),
        ifd_entry(TAG_IMAGE_LENGTH, TIFF_TYPE_LONG, 1, 1280),
    ] {
        raw.extend_from_slice(&entry);
    }
    raw.extend_from_slice(&0u32.to_le_bytes());

    let metadata = metadata_of("dng", &raw);
    assert_eq!((metadata.width, metadata.height), (Some(8368), Some(5584)));
}

#[cfg(feature = "raw-full")]
fn sensor_border_entries() -> Vec<Vec<u8>> {
    [
        (TAG_PANASONIC_SENSOR_WIDTH, 8152),
        (TAG_PANASONIC_SENSOR_HEIGHT, 5432),
        (TAG_PANASONIC_SENSOR_TOP_BORDER, 4),
        (TAG_PANASONIC_SENSOR_LEFT_BORDER, 4),
        (TAG_PANASONIC_SENSOR_BOTTOM_BORDER, 5428),
        (TAG_PANASONIC_SENSOR_RIGHT_BORDER, 8148),
    ]
    .into_iter()
    .map(|(tag, value)| ifd_entry(tag, TIFF_TYPE_SHORT, 1, value))
    .collect()
}

#[cfg(feature = "raw-full")]
#[test]
fn extracts_panasonic_rw2_dimensions_from_sensor_borders() {
    let raw = tiff(TIFF_MAGIC_RW2, &sensor_border_entries(), 0, &[]);
    let metadata = metadata_of("rw2", &raw);
    assert_eq!((metadata.width, metadata.height), (Some(8144), Some(5424)));
}

#[cfg(feature = "raw-full")]
#[test]
fn extracts_panasonic_rw2_container_entries_without_preview_fallback() {
    let mut entries = sensor_border_entries();
    entries.push(ifd_entry(
        TAG_PANASONIC_BITS_PER_SAMPLE,
        TIFF_TYPE_SHORT,
        1,
        14,
    ));
    entries.push(ifd_entry(TAG_PANASONIC_RAW_FORMAT, TIFF_TYPE_SHORT, 1, 8));
    entries.push(ifd_entry(
        TAG_PANASONIC_RAW_VERSION,
        TIFF_TYPE_UNDEFINED,
        4,
        u32::from_le_bytes(*b"0500"),
    ));
    let metadata = metadata_of("rw2", &tiff(TIFF_MAGIC_RW2, &entries, 0, &[]));
    assert!(has_entry(&metadata, "Bits Per Sample", "14"));
    assert!(has_entry(&metadata, "Panasonic Raw Version", "0500"));
    assert!(has_entry(&metadata, "Raw Format", "8"));
}

#[cfg(feature = "raw-full")]
#[test]
fn extracts_x3f_metadata_from_header4() {
    let mut x3f = vec![0u8; 768];
    x3f[0..4].copy_from_slice(X3F_SIGNATURE);
    x3f[4..8].copy_from_slice(&0x0004_0001u32.to_le_bytes());
    x3f[0x28..0x2c].copy_from_slice(&5424u32.to_le_bytes());
    x3f[0x2c..0x30].copy_from_slice(&3616u32.to_le_bytes());

    let metadata = metadata_of("x3f", &x3f);
    assert_eq!((metadata.width, metadata.height), (Some(5424), Some(3616)));
    assert!(has_entry(&metadata, "File Version", "4.1"));
    assert!(has_entry(&metadata, "Rotation", "0"));
}
