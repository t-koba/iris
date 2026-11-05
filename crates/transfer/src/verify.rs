use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::metadata::MetadataLevel;
use iris_core_domain::types::object::ObjectRef;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tracing::debug;

const VERIFY_HASH_BUFFER_SIZE: usize = 1024 * 1024;

#[derive(Debug)]
pub enum VerifyResult {
    Match,
    /// The copies definitely differ (for example, their sizes).
    Mismatch {
        reason: String,
    },
    /// Metadata differs in a way that does not prove the content differs: many destinations
    /// cannot keep the source mtime (S3 `LastModified` is the upload time, FAT keeps 2 s) and
    /// ETags are backend-specific (multipart uploads change them). Only a content hash decides.
    Inconclusive {
        reason: String,
    },
}

/// VerifyLight: compares size, then mtime and ETag.
///
/// A size difference is a [`VerifyResult::Mismatch`]; mtime or ETag differences are
/// [`VerifyResult::Inconclusive`] and must be settled by [`verify_strong`].
pub async fn verify_light(
    src_backend: &dyn BackendAdapter,
    src: &ObjectRef,
    dst_backend: &dyn BackendAdapter,
    dst: &ObjectRef,
) -> Result<VerifyResult, BackendError> {
    let src_meta = src_backend.get_metadata(src, MetadataLevel::L1).await?;
    let dst_meta = dst_backend.get_metadata(dst, MetadataLevel::L1).await?;

    if let (Some(src_size), Some(dst_size)) = (src_meta.size, dst_meta.size) {
        if src_size != dst_size {
            return Ok(VerifyResult::Mismatch {
                reason: format!("size mismatch: src={}, dst={}", src_size, dst_size),
            });
        }
    }

    // Only when both sides have an mtime.
    if let (Some(src_mtime), Some(dst_mtime)) = (src_meta.mtime, dst_meta.mtime) {
        if src_mtime != dst_mtime {
            return Ok(VerifyResult::Inconclusive {
                reason: format!("mtime mismatch: src={:?}, dst={:?}", src_mtime, dst_mtime),
            });
        }
    }

    // Only when both sides have an ETag.
    if let (Some(ref src_etag), Some(ref dst_etag)) = (&src_meta.etag, &dst_meta.etag) {
        if src_etag != dst_etag {
            return Ok(VerifyResult::Inconclusive {
                reason: format!("etag mismatch: src={}, dst={}", src_etag, dst_etag),
            });
        }
    }

    debug!("VerifyLight passed for {:?}", dst.display_path);
    Ok(VerifyResult::Match)
}

/// VerifyStrong: full hash comparison.
pub async fn verify_strong(
    src_backend: &dyn BackendAdapter,
    src: &ObjectRef,
    dst_backend: &dyn BackendAdapter,
    dst: &ObjectRef,
) -> Result<VerifyResult, BackendError> {
    let src_hash = compute_hash(src_backend, src).await?;
    let dst_hash = compute_hash(dst_backend, dst).await?;

    if src_hash != dst_hash {
        return Ok(VerifyResult::Mismatch {
            reason: format!("hash mismatch: src={}, dst={}", src_hash, dst_hash),
        });
    }

    debug!("VerifyStrong passed for {:?}", dst.display_path);
    Ok(VerifyResult::Match)
}

/// Whether VerifyLight must be followed by VerifyStrong.
pub fn should_escalate_to_strong(
    src_backend_id: &str,
    dst_backend_id: &str,
    light_result: &VerifyResult,
    user_requested: bool,
    high_risk_path: bool,
) -> bool {
    if user_requested {
        return true;
    }

    // Copies between different backends.
    if src_backend_id != dst_backend_id {
        return true;
    }

    if !matches!(light_result, VerifyResult::Match) {
        return true;
    }

    if high_risk_path {
        return true;
    }

    false
}

/// Runs VerifyLight and, when required or when it is inconclusive, VerifyStrong.
///
/// Returns `Ok(Err(reason))` when the copies differ, so callers keep the source.
pub async fn verify_copy(
    src_backend: &dyn BackendAdapter,
    src: &ObjectRef,
    dst_backend: &dyn BackendAdapter,
    dst: &ObjectRef,
    user_requested_strong: bool,
    high_risk_path: bool,
) -> Result<Result<(), String>, BackendError> {
    let light = verify_light(src_backend, src, dst_backend, dst).await?;
    let must_strong = should_escalate_to_strong(
        &src.key.backend_id.0,
        &dst.key.backend_id.0,
        &light,
        user_requested_strong,
        high_risk_path,
    );
    match light {
        VerifyResult::Mismatch { reason } => {
            return Ok(Err(format!("verify light failed: {reason}")));
        }
        VerifyResult::Inconclusive { reason } => {
            debug!("VerifyLight inconclusive ({reason}); escalating to VerifyStrong");
        }
        VerifyResult::Match => {}
    }
    if must_strong {
        match verify_strong(src_backend, src, dst_backend, dst).await? {
            VerifyResult::Match => {}
            VerifyResult::Mismatch { reason } | VerifyResult::Inconclusive { reason } => {
                return Ok(Err(format!("verify strong failed: {reason}")));
            }
        }
    }
    Ok(Ok(()))
}

async fn compute_hash(
    backend: &dyn BackendAdapter,
    object: &ObjectRef,
) -> Result<String, BackendError> {
    let handle = backend.read(object, None).await?;
    let mut reader = handle.stream;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; VERIFY_HASH_BUFFER_SIZE];

    loop {
        let n = reader
            .read(&mut buf)
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let hash = hasher.finalize();
    Ok(hash.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use iris_core_domain::types::capability::CapabilitySet;
    use iris_core_domain::types::list::{ListCursor, ListPage};
    use iris_core_domain::types::metadata::{MetadataLevel, MetadataResult};
    use iris_core_domain::types::object::{BackendId, ObjectKey, StableId};
    use iris_core_domain::types::operation::{OperationKind, OperationTicket, TicketId};
    use iris_core_domain::types::options::{
        ConflictResolution, CopyOptions, DeleteMode, MoveOptions,
    };
    use iris_core_domain::types::progress::ByteRange;
    use iris_core_domain::types::read_handle::ReadHandle;
    use iris_core_domain::types::session::BackendSession;
    use iris_core_domain::types::thumbnail::{ThumbnailHint, ThumbnailResult};
    use std::pin::Pin;
    use std::time::{Duration, SystemTime};
    use tokio::io::AsyncRead;

    struct MetadataOnlyBackend {
        metadata: MetadataResult,
    }

    #[async_trait]
    impl BackendAdapter for MetadataOnlyBackend {
        async fn connect(&self) -> Result<BackendSession, BackendError> {
            Ok(BackendSession {
                backend_id: BackendId("test".to_string()),
                display_name: "test".to_string(),
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
            Ok(self.metadata.clone())
        }

        async fn read(
            &self,
            _object: &ObjectRef,
            _range: Option<ByteRange>,
        ) -> Result<ReadHandle, BackendError> {
            let reader: Pin<Box<dyn AsyncRead + Send>> = Box::pin(tokio::io::empty());
            Ok(ReadHandle {
                stream: reader,
                content_length: Some(0),
                content_range: None,
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
            Ok(OperationTicket {
                id: TicketId(1),
                kind: OperationKind::Copy,
            })
        }

        async fn move_object(
            &self,
            _src: &ObjectRef,
            _dst: &ObjectRef,
            _options: MoveOptions,
        ) -> Result<OperationTicket, BackendError> {
            Ok(OperationTicket {
                id: TicketId(1),
                kind: OperationKind::Move,
            })
        }

        async fn delete(
            &self,
            _object: &ObjectRef,
            _mode: DeleteMode,
        ) -> Result<OperationTicket, BackendError> {
            Ok(OperationTicket {
                id: TicketId(1),
                kind: OperationKind::Delete,
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
            Ok(self.metadata.clone())
        }

        async fn get_stable_id(&self, object: &ObjectRef) -> Result<StableId, BackendError> {
            Ok(object.key.stable_id.clone())
        }

        fn capabilities(&self) -> CapabilitySet {
            CapabilitySet {
                can_range_read: true,
                can_range_write: false,
                can_direct_thumbnail: false,
                can_rename: true,
                can_clone: false,
                can_server_side_copy: false,
                can_provide_progress: true,
                can_verify_by_checksum: true,
                can_trash: true,
                can_etag: true,
            }
        }
    }

    fn object_ref() -> ObjectRef {
        ObjectRef {
            key: ObjectKey {
                backend_id: BackendId("test".to_string()),
                stable_id: StableId("object".to_string()),
            },
            parent_stable_id: None,
            display_path: "object".to_string(),
        }
    }

    fn metadata(size: u64, mtime: SystemTime, etag: &str) -> MetadataResult {
        MetadataResult {
            level: MetadataLevel::L1,
            is_dir: false,
            size: Some(size),
            mtime: Some(mtime),
            ctime: None,
            content_type: None,
            etag: Some(etag.to_string()),
        }
    }

    #[tokio::test]
    async fn verify_light_reports_size_difference_as_mismatch() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let src_backend = MetadataOnlyBackend {
            metadata: metadata(123, base, "etag"),
        };
        let dst_backend = MetadataOnlyBackend {
            metadata: metadata(124, base, "etag"),
        };

        let result = verify_light(&src_backend, &object_ref(), &dst_backend, &object_ref())
            .await
            .expect("verify_light should succeed");
        assert!(matches!(result, VerifyResult::Mismatch { reason } if reason.contains("size")));
    }

    #[tokio::test]
    async fn verify_light_etag_difference_is_inconclusive() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let src_backend = MetadataOnlyBackend {
            metadata: metadata(123, base, "single-part"),
        };
        let dst_backend = MetadataOnlyBackend {
            metadata: metadata(123, base, "multipart-2"),
        };

        let result = verify_light(&src_backend, &object_ref(), &dst_backend, &object_ref())
            .await
            .expect("verify_light should succeed");
        assert!(matches!(result, VerifyResult::Inconclusive { reason } if reason.contains("etag")));
    }

    #[tokio::test]
    async fn verify_copy_settles_metadata_differences_by_content_hash() {
        // Destinations such as S3 report the upload time instead of the source mtime.
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let src_backend = MetadataOnlyBackend {
            metadata: metadata(0, base, "etag"),
        };
        let dst_backend = MetadataOnlyBackend {
            metadata: metadata(0, base + Duration::from_secs(3600), "etag"),
        };

        let verdict = verify_copy(
            &src_backend,
            &object_ref(),
            &dst_backend,
            &object_ref(),
            false,
            false,
        )
        .await
        .expect("verify should run");
        assert_eq!(verdict, Ok(()));
    }

    #[tokio::test]
    async fn verify_light_detects_mtime_mismatch() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let src_backend = MetadataOnlyBackend {
            metadata: MetadataResult {
                level: MetadataLevel::L1,
                is_dir: false,
                size: Some(123),
                mtime: Some(base),
                ctime: None,
                content_type: None,
                etag: Some("etag".to_string()),
            },
        };
        let dst_backend = MetadataOnlyBackend {
            metadata: MetadataResult {
                level: MetadataLevel::L1,
                is_dir: false,
                size: Some(123),
                mtime: Some(base + Duration::from_secs(1)),
                ctime: None,
                content_type: None,
                etag: Some("etag".to_string()),
            },
        };

        let result = verify_light(&src_backend, &object_ref(), &dst_backend, &object_ref())
            .await
            .expect("verify_light should succeed");
        assert!(
            matches!(result, VerifyResult::Inconclusive { reason } if reason.contains("mtime"))
        );
    }

    #[test]
    fn high_risk_path_escalates_to_strong_verify() {
        assert!(should_escalate_to_strong(
            "local",
            "local",
            &VerifyResult::Match,
            false,
            true,
        ));
    }

    #[test]
    fn strong_verify_hash_buffer_is_one_mib() {
        assert_eq!(VERIFY_HASH_BUFFER_SIZE, 1024 * 1024);
    }
}
