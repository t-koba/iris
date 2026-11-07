use super::*;
use crate::test_fixtures::{jpeg, object, temp_path, MemoryBackend};
use std::fs::File;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

const TAG_IMAGE_WIDTH: u16 = 0x0100;
const TAG_IMAGE_LENGTH: u16 = 0x0101;
const TAG_MODEL: u16 = 0x0110;

fn tiff_ifd_with_model(entries: Vec<(u16, u16, u32, u32)>, extra: &[u8]) -> Vec<u8> {
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&42u16.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, field_type, count, value_or_offset) in entries {
        tiff.extend_from_slice(&tag.to_le_bytes());
        tiff.extend_from_slice(&field_type.to_le_bytes());
        tiff.extend_from_slice(&count.to_le_bytes());
        tiff.extend_from_slice(&value_or_offset.to_le_bytes());
    }
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff.extend_from_slice(extra);
    tiff
}

fn jpeg_with_exif_model(width: u32, height: u32, model: &str) -> Vec<u8> {
    let jpeg = jpeg(width, height);
    let model_bytes = format!("{model}\0").into_bytes();
    let string_offset = 8 + 2 + 12 + 4;
    let tiff = tiff_ifd_with_model(
        vec![(TAG_MODEL, 2, model_bytes.len() as u32, string_offset as u32)],
        &model_bytes,
    );
    let mut app1_payload = b"Exif\0\0".to_vec();
    app1_payload.extend_from_slice(&tiff);

    let mut output = Vec::with_capacity(jpeg.len() + app1_payload.len() + 4);
    output.extend_from_slice(&jpeg[..2]);
    output.extend_from_slice(&[0xFF, 0xE1]);
    output.extend_from_slice(&((app1_payload.len() + 2) as u16).to_be_bytes());
    output.extend_from_slice(&app1_payload);
    output.extend_from_slice(&jpeg[2..]);
    output
}

fn dng_with_tiff_metadata(width: u32, height: u32, model: &str) -> Vec<u8> {
    let model_bytes = format!("{model}\0").into_bytes();
    let string_offset = 8 + 2 + (3 * 12) + 4;
    tiff_ifd_with_model(
        vec![
            (TAG_IMAGE_WIDTH, 4, 1, width),
            (TAG_IMAGE_LENGTH, 4, 1, height),
            (TAG_MODEL, 2, model_bytes.len() as u32, string_offset as u32),
        ],
        &model_bytes,
    )
}

#[tokio::test]
async fn extracts_remote_jpeg_metadata_via_range_reads() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(MemoryBackend {
        bytes: jpeg_with_exif_model(160, 90, "IRIS-CAM"),
        reads: reads.clone(),
        content_type: Some("image/jpeg".to_string()),
        ..MemoryBackend::default()
    });

    let metadata = extract_image_metadata_from_backend(backend, object("photo.jpg"))
        .await
        .expect("remote jpeg metadata");

    assert_eq!(metadata.width, Some(160));
    assert_eq!(metadata.height, Some(90));
    assert_eq!(metadata.media.width, Some(160));
    assert_eq!(metadata.media.height, Some(90));
    assert_eq!(metadata.media.camera_model.as_deref(), Some("IRIS-CAM"));
    assert!(metadata
        .exif_entries
        .iter()
        .any(|(tag, value)| tag.contains("Model") && value.contains("IRIS-CAM")));

    let reads = reads.lock().expect("lock reads");
    assert!(!reads.is_empty(), "expected partial reads");
    assert!(reads.iter().all(Option::is_some), "full read fallback used");
}

#[tokio::test]
async fn extracts_remote_raw_tiff_metadata_via_range_reads() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(MemoryBackend {
        bytes: dng_with_tiff_metadata(4000, 3000, "REMOTE-DNG"),
        reads: reads.clone(),
        content_type: Some("image/x-adobe-dng".to_string()),
        ..MemoryBackend::default()
    });

    let metadata = extract_image_metadata_from_backend(backend, object("sample.dng"))
        .await
        .expect("remote dng metadata");

    assert_eq!(metadata.width, Some(4000));
    assert_eq!(metadata.height, Some(3000));
    assert_eq!(metadata.media.width, Some(4000));
    assert_eq!(metadata.media.height, Some(3000));
    assert_eq!(metadata.media.camera_model.as_deref(), Some("REMOTE-DNG"));
    assert!(metadata
        .exif_entries
        .iter()
        .any(|(tag, value)| tag.contains("Model") && value.contains("REMOTE-DNG")));

    let reads = reads.lock().expect("lock reads");
    assert!(!reads.is_empty(), "expected partial reads");
    assert!(reads.iter().all(Option::is_some), "full read fallback used");
}

#[test]
fn extract_local_raw_metadata_does_not_modify_source_mtime() {
    let path = temp_path("dng");
    std::fs::write(&path, dng_with_tiff_metadata(4000, 3000, "LOCAL-DNG"))
        .expect("write raw metadata file");

    let expected_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    File::options()
        .write(true)
        .open(&path)
        .expect("open raw metadata file")
        .set_times(std::fs::FileTimes::new().set_modified(expected_mtime))
        .expect("set raw mtime");

    let metadata = extract_image_metadata_from_path(&path);

    assert_eq!(metadata.width, Some(4000));
    assert_eq!(metadata.height, Some(3000));
    assert_eq!(metadata.media.width, Some(4000));
    assert_eq!(metadata.media.height, Some(3000));
    assert_eq!(metadata.media.camera_model.as_deref(), Some("LOCAL-DNG"));
    assert!(metadata
        .exif_entries
        .iter()
        .any(|(tag, value)| tag.contains("Model") && value.contains("LOCAL-DNG")));
    let stat = std::fs::metadata(&path).expect("stat raw metadata file");
    assert_eq!(stat.modified().ok(), Some(expected_mtime));
    let _ = std::fs::remove_file(path);
}

#[test]
fn typed_exif_helpers_parse_capture_time_offset_and_gps() {
    assert_eq!(
        parse_exif_datetime("2024:05:06 07:08:09".to_string()),
        Some(1_714_979_289)
    );
    assert_eq!(parse_offset_minutes("+09:30".to_string()), Some(570));
    assert_eq!(parse_offset_minutes("-03:15".to_string()), Some(-195));

    let value = exif::Value::Rational(vec![
        exif::Rational { num: 35, denom: 1 },
        exif::Rational { num: 39, denom: 1 },
        exif::Rational {
            num: 1234,
            denom: 100,
        },
    ]);
    let degrees = gps_dms_value(&value).expect("gps dms");
    assert!((degrees - 35.65342777777778).abs() < 0.0000001);
    assert_eq!(apply_gps_ref(139.0, Some("W")), -139.0);
}
