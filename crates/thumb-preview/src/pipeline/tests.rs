use super::*;
use async_trait::async_trait;
use iris_core_domain::types::capability::CapabilitySet;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::list::{ListCursor, ListPage};
use iris_core_domain::types::metadata::{MetadataLevel, MetadataResult};
use iris_core_domain::types::object::{BackendId, ObjectKey, StableId};
use iris_core_domain::types::operation::{OperationKind, OperationTicket, TicketId};
use iris_core_domain::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use iris_core_domain::types::progress::ByteRange;
use iris_core_domain::types::read_handle::ReadHandle;
use iris_core_domain::types::session::BackendSession;
use std::fs::File;
use std::io::Cursor;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::io::AsyncRead;

#[test]
fn classify_raw_by_extension_when_mime_missing() {
    let file_type = classify_for_local_path(Path::new("/tmp/sample.x3f"), None);
    assert!(matches!(file_type, FileType::Raw));
}

#[test]
fn classify_raw_by_extension_case_insensitive() {
    let file_type = classify_for_local_path(Path::new("/tmp/sample.X3F"), None);
    assert!(matches!(file_type, FileType::Raw));
}

#[test]
fn classify_all_fr032_raw_extensions() {
    let extensions = [
        "arw", "cr2", "cr3", "dng", "nef", "orf", "raf", "rw2", "x3f",
    ];
    for ext in extensions {
        let file = format!("/tmp/sample.{}", ext);
        let file_type = classify_for_local_path(Path::new(&file), None);
        assert!(matches!(file_type, FileType::Raw), "not raw: {}", ext);
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
        assert!(matches!(file_type, FileType::Image), "not image: {ext}");
    }
}

#[test]
fn classify_motion_jpeg2000_as_video() {
    assert!(matches!(
        classify_for_local_path(Path::new("/tmp/sample.mj2"), None),
        FileType::Video
    ));
}

#[test]
fn classify_jpeg_xr_vendor_mime_as_image() {
    assert!(matches!(
        classify_content_type(Some("image/vnd.ms-photo")),
        FileType::Image
    ));
}

struct MockReadBackend {
    bytes: Vec<u8>,
    reads: Arc<Mutex<Vec<Option<ByteRange>>>>,
}

struct MockRangeRawBackend {
    bytes: Vec<u8>,
    reads: Arc<Mutex<Vec<Option<ByteRange>>>>,
}

#[async_trait]
impl BackendAdapter for MockReadBackend {
    async fn connect(&self) -> Result<BackendSession, BackendError> {
        Ok(BackendSession {
            backend_id: BackendId("mock".to_string()),
            display_name: "mock".to_string(),
        })
    }

    async fn list_names(
        &self,
        _dir: &ObjectRef,
        _cursor: Option<ListCursor>,
    ) -> Result<ListPage, BackendError> {
        Err(BackendError::Unsupported { operation: "list" })
    }

    async fn stat(&self, _object: &ObjectRef) -> Result<MetadataResult, BackendError> {
        Err(BackendError::Unsupported { operation: "stat" })
    }

    async fn read(
        &self,
        _object: &ObjectRef,
        range: Option<ByteRange>,
    ) -> Result<ReadHandle, BackendError> {
        self.reads.lock().expect("lock reads").push(range.clone());
        let bytes = if let Some(range) = &range {
            let start = usize::try_from(range.offset).unwrap_or(usize::MAX);
            let requested_end = range
                .length
                .and_then(|length| range.offset.checked_add(length))
                .and_then(|end| usize::try_from(end).ok())
                .unwrap_or(self.bytes.len());
            self.bytes
                .get(start.min(self.bytes.len())..requested_end.min(self.bytes.len()))
                .unwrap_or_default()
                .to_vec()
        } else {
            self.bytes.clone()
        };
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp_path = std::env::temp_dir().join(format!(
            "iris-thumb-preview-read-{}-{}.bin",
            std::process::id(),
            unique_id
        ));
        std::fs::write(&temp_path, &bytes)
            .map_err(|error| BackendError::Internal(Box::new(error)))?;
        let reader: Pin<Box<dyn AsyncRead + Send>> = Box::pin(
            tokio::fs::File::open(&temp_path)
                .await
                .map_err(|error| BackendError::Internal(Box::new(error)))?,
        );
        Ok(ReadHandle {
            stream: reader,
            content_length: Some(bytes.len() as u64),
            content_range: range,
        })
    }

    async fn write_object(
        &self,
        _dst: &ObjectRef,
        _data: Vec<u8>,
        _on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        Ok(OperationTicket {
            id: TicketId(1),
            kind: OperationKind::Copy,
        })
    }

    async fn write_from_reader(
        &self,
        _dst: &ObjectRef,
        _reader: Pin<Box<dyn AsyncRead + Send>>,
        _content_length: Option<u64>,
        _on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        Ok(OperationTicket {
            id: TicketId(1),
            kind: OperationKind::Copy,
        })
    }

    async fn mkdir(&self, _parent: &ObjectRef, _name: &str) -> Result<ObjectRef, BackendError> {
        Err(BackendError::Unsupported { operation: "mkdir" })
    }

    async fn copy(
        &self,
        _src: &ObjectRef,
        _dst: &ObjectRef,
        _options: CopyOptions,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported { operation: "copy" })
    }

    async fn move_object(
        &self,
        _src: &ObjectRef,
        _dst: &ObjectRef,
        _options: MoveOptions,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported {
            operation: "move_object",
        })
    }

    async fn delete(
        &self,
        _object: &ObjectRef,
        _mode: DeleteMode,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported {
            operation: "delete",
        })
    }

    async fn get_thumbnail(
        &self,
        _object: &ObjectRef,
        _hint: ThumbnailHint,
    ) -> Result<ThumbnailResult, BackendError> {
        Err(BackendError::Unsupported {
            operation: "get_thumbnail",
        })
    }

    async fn get_metadata(
        &self,
        _object: &ObjectRef,
        _level: MetadataLevel,
    ) -> Result<MetadataResult, BackendError> {
        Err(BackendError::Unsupported {
            operation: "get_metadata",
        })
    }

    async fn get_stable_id(&self, object: &ObjectRef) -> Result<StableId, BackendError> {
        Ok(object.key.stable_id.clone())
    }

    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet {
            can_range_read: true,
            can_range_write: false,
            can_direct_thumbnail: false,
            can_rename: false,
            can_clone: false,
            can_server_side_copy: false,
            can_provide_progress: false,
            can_verify_by_checksum: false,
            can_trash: false,
            can_etag: false,
        }
    }
}

#[async_trait]
impl BackendAdapter for MockRangeRawBackend {
    async fn connect(&self) -> Result<BackendSession, BackendError> {
        Ok(BackendSession {
            backend_id: BackendId("mock".to_string()),
            display_name: "mock".to_string(),
        })
    }

    async fn list_names(
        &self,
        _dir: &ObjectRef,
        _cursor: Option<ListCursor>,
    ) -> Result<ListPage, BackendError> {
        Err(BackendError::Unsupported { operation: "list" })
    }

    async fn stat(&self, _object: &ObjectRef) -> Result<MetadataResult, BackendError> {
        Ok(MetadataResult {
            level: MetadataLevel::L1,
            is_dir: false,
            size: Some(self.bytes.len() as u64),
            mtime: None,
            ctime: None,
            content_type: Some("image/x-adobe-dng".to_string()),
            etag: None,
        })
    }

    async fn read(
        &self,
        _object: &ObjectRef,
        range: Option<ByteRange>,
    ) -> Result<ReadHandle, BackendError> {
        self.reads.lock().expect("lock reads").push(range.clone());
        let Some(range) = range else {
            return Err(BackendError::Unsupported {
                operation: "full read should not be used for RAW test",
            });
        };

        let start = range.offset as usize;
        let len: usize = range
            .length
            .expect("range length")
            .try_into()
            .expect("usize length");
        let end = start + len;
        let slice = self
            .bytes
            .get(start..end)
            .expect("requested in-range bytes")
            .to_vec();

        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp_path = std::env::temp_dir().join(format!(
            "iris-thumb-preview-range-{}-{}.bin",
            std::process::id(),
            unique_id
        ));
        std::fs::write(&temp_path, &slice)
            .map_err(|error| BackendError::Internal(Box::new(error)))?;
        let reader: Pin<Box<dyn AsyncRead + Send>> = Box::pin(
            tokio::fs::File::open(&temp_path)
                .await
                .map_err(|error| BackendError::Internal(Box::new(error)))?,
        );
        Ok(ReadHandle {
            stream: reader,
            content_length: Some(slice.len() as u64),
            content_range: Some(range),
        })
    }

    async fn write_object(
        &self,
        _dst: &ObjectRef,
        _data: Vec<u8>,
        _on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        Ok(OperationTicket {
            id: TicketId(1),
            kind: OperationKind::Copy,
        })
    }

    async fn write_from_reader(
        &self,
        _dst: &ObjectRef,
        _reader: Pin<Box<dyn AsyncRead + Send>>,
        _content_length: Option<u64>,
        _on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        Ok(OperationTicket {
            id: TicketId(1),
            kind: OperationKind::Copy,
        })
    }

    async fn mkdir(&self, _parent: &ObjectRef, _name: &str) -> Result<ObjectRef, BackendError> {
        Err(BackendError::Unsupported { operation: "mkdir" })
    }

    async fn copy(
        &self,
        _src: &ObjectRef,
        _dst: &ObjectRef,
        _options: CopyOptions,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported { operation: "copy" })
    }

    async fn move_object(
        &self,
        _src: &ObjectRef,
        _dst: &ObjectRef,
        _options: MoveOptions,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported {
            operation: "move_object",
        })
    }

    async fn delete(
        &self,
        _object: &ObjectRef,
        _mode: DeleteMode,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported {
            operation: "delete",
        })
    }

    async fn get_thumbnail(
        &self,
        _object: &ObjectRef,
        _hint: ThumbnailHint,
    ) -> Result<ThumbnailResult, BackendError> {
        Err(BackendError::Unsupported {
            operation: "get_thumbnail",
        })
    }

    async fn get_metadata(
        &self,
        _object: &ObjectRef,
        _level: MetadataLevel,
    ) -> Result<MetadataResult, BackendError> {
        Err(BackendError::Unsupported {
            operation: "get_metadata",
        })
    }

    async fn get_stable_id(&self, object: &ObjectRef) -> Result<StableId, BackendError> {
        Ok(object.key.stable_id.clone())
    }

    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet {
            can_range_read: true,
            can_range_write: false,
            can_direct_thumbnail: false,
            can_rename: false,
            can_clone: false,
            can_server_side_copy: false,
            can_provide_progress: false,
            can_verify_by_checksum: false,
            can_trash: false,
            can_etag: false,
        }
    }
}

fn encoded_test_jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut jpeg = Vec::new();
    image::DynamicImage::new_rgb8(width, height)
        .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .expect("jpeg encode");
    jpeg
}

fn structured_dng_with_embedded_jpeg(jpeg: &[u8]) -> Vec<u8> {
    let jpeg_offset = 64u32;
    let jpeg_len = jpeg.len() as u32;

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&42u16.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes());

    tiff.extend_from_slice(&2u16.to_le_bytes());
    tiff.extend_from_slice(&0x0201u16.to_le_bytes());
    tiff.extend_from_slice(&4u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&jpeg_offset.to_le_bytes());
    tiff.extend_from_slice(&0x0202u16.to_le_bytes());
    tiff.extend_from_slice(&4u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&jpeg_len.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());

    if tiff.len() < jpeg_offset as usize {
        tiff.resize(jpeg_offset as usize, 0);
    }
    tiff.extend_from_slice(jpeg);
    tiff
}

fn unique_local_test_path(ext: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!("iris-thumb-preview-local-{}.{}", nanos, ext))
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
    let backend = MockReadBackend {
        bytes: png_bytes,
        reads: reads.clone(),
    };
    let object = ObjectRef {
        key: ObjectKey {
            backend_id: BackendId("mock".to_string()),
            stable_id: StableId("image.png".to_string()),
        },
        parent_stable_id: None,
        display_path: "image.png".to_string(),
    };
    let hint = ThumbnailHint {
        max_dimension: 32,
        preferred_formats: vec![iris_core_domain::types::thumbnail::ImageFormat::Png],
    };

    let result = ThumbnailPipeline::generate(&backend, &object, &hint, Some("image/png"))
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
    let backend = MockReadBackend {
        bytes: image_bytes,
        reads: reads.clone(),
    };
    let object = ObjectRef {
        key: ObjectKey {
            backend_id: BackendId("mock".to_string()),
            stable_id: StableId("image.bmp".to_string()),
        },
        parent_stable_id: None,
        display_path: "image.bmp".to_string(),
    };
    let hint = ThumbnailHint {
        max_dimension: 32,
        preferred_formats: vec![iris_core_domain::types::thumbnail::ImageFormat::Png],
    };

    let result = ThumbnailPipeline::generate(&backend, &object, &hint, Some("image/bmp"))
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
    let raw_bytes = structured_dng_with_embedded_jpeg(&encoded_test_jpeg(96, 64));
    let reads = Arc::new(Mutex::new(Vec::new()));
    let backend = MockRangeRawBackend {
        bytes: raw_bytes,
        reads: reads.clone(),
    };
    let object = ObjectRef {
        key: ObjectKey {
            backend_id: BackendId("mock".to_string()),
            stable_id: StableId("sample.dng".to_string()),
        },
        parent_stable_id: None,
        display_path: "sample.dng".to_string(),
    };
    let hint = ThumbnailHint {
        max_dimension: 128,
        preferred_formats: vec![iris_core_domain::types::thumbnail::ImageFormat::Jpeg],
    };

    let result = ThumbnailPipeline::generate(&backend, &object, &hint, Some("image/x-adobe-dng"))
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
    let backend = MockRangeRawBackend {
        bytes: encoded_test_jpeg(96, 64),
        reads: reads.clone(),
    };
    let object = ObjectRef {
        key: ObjectKey {
            backend_id: BackendId("mock".to_string()),
            stable_id: StableId("missing-preview.dng".to_string()),
        },
        parent_stable_id: None,
        display_path: "missing-preview.dng".to_string(),
    };
    let hint = ThumbnailHint {
        max_dimension: 128,
        preferred_formats: vec![iris_core_domain::types::thumbnail::ImageFormat::Jpeg],
    };

    let result =
        ThumbnailPipeline::generate(&backend, &object, &hint, Some("image/x-adobe-dng")).await;

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
    let path = unique_local_test_path("dng");
    std::fs::write(
        &path,
        structured_dng_with_embedded_jpeg(&encoded_test_jpeg(96, 64)),
    )
    .expect("write raw");

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
    let result = ThumbnailPipeline::generate_from_path(&path, &hint, Some("image/x-adobe-dng"));

    assert!(result.is_some(), "thumbnail should be generated");
    let metadata = std::fs::metadata(&path).expect("stat raw");
    assert_eq!(metadata.modified().ok(), Some(expected_mtime));
    let _ = std::fs::remove_file(path);
}
