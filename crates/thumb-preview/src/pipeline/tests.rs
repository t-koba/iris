use super::*;
use crate::test_fixtures::{jpeg, object, temp_path, tiff_with_jpeg, MemoryBackend};
use iris_core_domain::types::error::BackendError;
use std::fs::File;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio_util::sync::CancellationToken;

#[test]
fn classify_raw_by_extension_when_mime_missing() {
    let file_type = classify_for_local_path(Path::new("/tmp/sample.x3f"), None);
    assert!(matches!(file_type, Some(MediaFormat::Raw)));
}

#[test]
fn classify_raw_by_extension_case_insensitive() {
    let file_type = classify_for_local_path(Path::new("/tmp/sample.X3F"), None);
    assert!(matches!(file_type, Some(MediaFormat::Raw)));
}

#[test]
fn classify_all_fr032_raw_extensions() {
    let extensions = [
        "arw", "cr2", "cr3", "dng", "nef", "orf", "raf", "rw2", "x3f",
    ];
    for ext in extensions {
        let file = format!("/tmp/sample.{}", ext);
        let file_type = classify_for_local_path(Path::new(&file), None);
        assert!(
            matches!(file_type, Some(MediaFormat::Raw)),
            "not raw: {}",
            ext
        );
    }
}

#[test]
fn classify_all_modern_image_extensions() {
    let extensions = [
        "avif", "heic", "heif", "hif", "jp2", "j2k", "j2c", "jpc", "jpf", "jpx", "jpm", "jxr",
        "hdp", "wdp", "jxl",
    ];
    for ext in extensions {
        let file = format!("/tmp/sample.{ext}");
        let file_type = classify_for_local_path(Path::new(&file), None);
        assert!(
            matches!(
                file_type,
                Some(MediaFormat::Image | MediaFormat::ModernImage)
            ),
            "not image: {ext}"
        );
    }
}

#[test]
fn classify_motion_jpeg2000_as_video() {
    assert!(matches!(
        classify_for_local_path(Path::new("/tmp/sample.mj2"), None),
        Some(MediaFormat::Video)
    ));
}

#[test]
fn classify_jpeg_xr_vendor_mime_as_image() {
    assert!(matches!(
        classify_content_type(Some("image/vnd.ms-photo")),
        Some(MediaFormat::Image | MediaFormat::ModernImage)
    ));
}

#[tokio::test]
async fn generate_does_not_redownload_a_complete_short_remote_probe() {
    let image = image::DynamicImage::new_rgba8(1, 1);
    let mut png_bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .expect("encode png");
    let expected_len = png_bytes.len();
    let reads = Arc::new(Mutex::new(Vec::new()));
    let backend = MemoryBackend {
        bytes: png_bytes,
        reads: reads.clone(),
        content_type: None,
    };
    let object = object("image.png");
    let hint = ThumbnailHint {
        max_dimension: 32,
        preferred_formats: vec![iris_core_domain::types::thumbnail::ImageFormat::Png],
    };

    let result = ThumbnailPipeline::generate(
        &backend,
        &object,
        &hint,
        Some("image/png"),
        &CancellationToken::new(),
    )
    .await
    .expect("generate thumbnail from temp file");
    assert_eq!(result.width, 1);
    assert_eq!(result.height, 1);
    assert_eq!(result.data.len(), expected_len);
    let reads = reads.lock().expect("lock reads");
    assert_eq!(reads.len(), 1);
    assert_eq!(
        reads[0].as_ref().and_then(|range| range.length),
        Some(128 * 1024)
    );
}

#[tokio::test]
async fn generate_requests_only_the_tail_after_a_large_remote_probe() {
    let image = image::DynamicImage::new_rgb8(512, 512);
    let mut image_bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut image_bytes),
            image::ImageFormat::Bmp,
        )
        .expect("encode bmp");
    assert!(image_bytes.len() > 128 * 1024);
    let reads = Arc::new(Mutex::new(Vec::new()));
    let backend = MemoryBackend {
        bytes: image_bytes,
        reads: reads.clone(),
        content_type: None,
    };
    let object = object("image.bmp");
    let hint = ThumbnailHint {
        max_dimension: 32,
        preferred_formats: vec![iris_core_domain::types::thumbnail::ImageFormat::Png],
    };

    let result = ThumbnailPipeline::generate(
        &backend,
        &object,
        &hint,
        Some("image/bmp"),
        &CancellationToken::new(),
    )
    .await
    .expect("generate thumbnail from range reads");
    assert_eq!((result.width, result.height), (32, 32));

    let reads = reads.lock().expect("lock reads");
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0].as_ref().map(|range| range.offset), Some(0));
    assert_eq!(
        reads[0].as_ref().and_then(|range| range.length),
        Some(128 * 1024)
    );
    assert_eq!(
        reads[1].as_ref().map(|range| range.offset),
        Some(128 * 1024)
    );
    assert_eq!(reads[1].as_ref().and_then(|range| range.length), None);
}

#[tokio::test]
async fn generate_raw_thumbnail_via_structured_range_reads() {
    let raw_bytes = tiff_with_jpeg(64, &jpeg(96, 64));
    let reads = Arc::new(Mutex::new(Vec::new()));
    let backend = MemoryBackend {
        bytes: raw_bytes,
        reads: reads.clone(),
        content_type: Some("image/x-adobe-dng".to_string()),
    };
    let object = object("sample.dng");
    let hint = ThumbnailHint {
        max_dimension: 128,
        preferred_formats: vec![iris_core_domain::types::thumbnail::ImageFormat::Jpeg],
    };

    let result = ThumbnailPipeline::generate(
        &backend,
        &object,
        &hint,
        Some("image/x-adobe-dng"),
        &CancellationToken::new(),
    )
    .await
    .expect("generate structured RAW thumbnail");

    assert_eq!((result.width, result.height), (96, 64));
    let reads = reads.lock().expect("lock reads");
    assert!(!reads.is_empty(), "expected partial reads");
    assert!(reads.iter().all(Option::is_some), "full read fallback used");
    assert!(
        reads.len() <= 2,
        "too many RAW range reads: {}",
        reads.len()
    );
}

#[tokio::test]
async fn generate_raw_without_embedded_preview_returns_unsupported() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let backend = MemoryBackend {
        bytes: jpeg(96, 64),
        reads: reads.clone(),
        content_type: Some("image/x-adobe-dng".to_string()),
    };
    let object = object("missing-preview.dng");
    let hint = ThumbnailHint {
        max_dimension: 128,
        preferred_formats: vec![iris_core_domain::types::thumbnail::ImageFormat::Jpeg],
    };

    let result = ThumbnailPipeline::generate(
        &backend,
        &object,
        &hint,
        Some("image/x-adobe-dng"),
        &CancellationToken::new(),
    )
    .await;

    assert!(matches!(
        result,
        Err(BackendError::Unsupported {
            operation: "raw embedded preview missing",
        })
    ));
    let reads = reads.lock().expect("lock reads");
    assert!(reads.iter().all(Option::is_some), "full read fallback used");
}

#[test]
fn generate_local_raw_thumbnail_does_not_modify_source_mtime() {
    let path = temp_path("dng");
    std::fs::write(&path, tiff_with_jpeg(64, &jpeg(96, 64))).expect("write raw");

    let expected_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    File::options()
        .write(true)
        .open(&path)
        .expect("open raw")
        .set_times(std::fs::FileTimes::new().set_modified(expected_mtime))
        .expect("set raw mtime");

    let hint = ThumbnailHint {
        max_dimension: 128,
        preferred_formats: vec![iris_core_domain::types::thumbnail::ImageFormat::Jpeg],
    };
    let result = ThumbnailPipeline::generate_from_path(
        &path,
        &hint,
        Some("image/x-adobe-dng"),
        &CancellationToken::new(),
    );

    assert!(result.is_ok(), "thumbnail should be generated");
    let metadata = std::fs::metadata(&path).expect("stat raw");
    assert_eq!(metadata.modified().ok(), Some(expected_mtime));
    let _ = std::fs::remove_file(path);
}
