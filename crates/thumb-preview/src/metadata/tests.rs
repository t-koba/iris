use super::*;
use async_trait::async_trait;
use iris_core_domain::types::capability::CapabilitySet;
use iris_core_domain::types::list::{ListCursor, ListPage};
use iris_core_domain::types::metadata::{MetadataLevel, MetadataResult};
use iris_core_domain::types::object::{BackendId, ObjectKey, StableId};
use iris_core_domain::types::operation::{OperationKind, OperationTicket, TicketId};
use iris_core_domain::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use iris_core_domain::types::progress::ByteRange;
use iris_core_domain::types::read_handle::ReadHandle;
use iris_core_domain::types::session::BackendSession;
use std::fs::File;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::io::AsyncRead;

const TAG_IMAGE_WIDTH: u16 = 0x0100;
const TAG_IMAGE_LENGTH: u16 = 0x0101;
const TAG_MODEL: u16 = 0x0110;

struct MockRangeMetadataBackend {
    bytes: Vec<u8>,
    reads: Arc<Mutex<Vec<Option<ByteRange>>>>,
    content_type: Option<String>,
}

#[async_trait]
impl BackendAdapter for MockRangeMetadataBackend {
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
            content_type: self.content_type.clone(),
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
                operation: "full read should not be used for metadata test",
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
            "iris-thumb-preview-metadata-range-{}-{}.bin",
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
        _hint: iris_core_domain::types::thumbnail::ThumbnailHint,
    ) -> Result<iris_core_domain::types::thumbnail::ThumbnailResult, BackendError> {
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
        .write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        )
        .expect("jpeg encode");
    jpeg
}

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
    let jpeg = encoded_test_jpeg(width, height);
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

fn unique_local_test_path(ext: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!("iris-thumb-metadata-local-{}.{}", nanos, ext))
}

fn object(path: &str) -> ObjectRef {
    ObjectRef {
        key: ObjectKey {
            backend_id: BackendId("mock".to_string()),
            stable_id: StableId(path.to_string()),
        },
        parent_stable_id: None,
        display_path: path.to_string(),
    }
}

#[tokio::test]
async fn extracts_remote_jpeg_metadata_via_range_reads() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(MockRangeMetadataBackend {
        bytes: jpeg_with_exif_model(160, 90, "IRIS-CAM"),
        reads: reads.clone(),
        content_type: Some("image/jpeg".to_string()),
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
    let backend = Arc::new(MockRangeMetadataBackend {
        bytes: dng_with_tiff_metadata(4000, 3000, "REMOTE-DNG"),
        reads: reads.clone(),
        content_type: Some("image/x-adobe-dng".to_string()),
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
    let path = unique_local_test_path("dng");
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
