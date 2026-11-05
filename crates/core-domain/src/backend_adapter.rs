use async_trait::async_trait;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::memory_budget::MemoryBudget;
use crate::types::capability::CapabilitySet;
use crate::types::error::BackendError;
use crate::types::list::{ListCursor, ListPage};
use crate::types::metadata::{MetadataLevel, MetadataResult};
use crate::types::multipart::{MultipartPart, MultipartUploadSession};
use crate::types::object::{ObjectRef, StableId};
use crate::types::operation::OperationTicket;
use crate::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use crate::types::progress::ByteRange;
use crate::types::read_handle::ReadHandle;
use crate::types::session::BackendSession;
use crate::types::thumbnail::{ThumbnailHint, ThumbnailResult};

const WRITE_FROM_READER_MISSING_LENGTH_OPERATION: &str =
    "write_from_reader requires content length";
const WRITE_FROM_READER_MEMORY_BUDGET_OPERATION: &str =
    "write_from_reader buffer exceeds memory budget";
const WRITE_FROM_READER_LENGTH_EXCEEDED_OPERATION: &str =
    "write_from_reader stream exceeds content length";
const WRITE_FROM_READER_BUFFER_BYTES: usize = 8 * 1024;

/// Common trait implemented by every storage backend.
#[async_trait]
pub trait BackendAdapter: Send + Sync {
    async fn connect(&self) -> Result<BackendSession, BackendError>;

    /// Name listing (stage A). Size and timestamps are resolved lazily via stat/get_metadata.
    /// A `None` cursor starts from the beginning.
    async fn list_names(
        &self,
        dir: &ObjectRef,
        cursor: Option<ListCursor>,
    ) -> Result<ListPage, BackendError>;

    /// L1 metadata (size, mtime, kind).
    async fn stat(&self, object: &ObjectRef) -> Result<MetadataResult, BackendError>;

    /// Reads the whole object, or only `range` when given.
    async fn read(
        &self,
        object: &ObjectRef,
        range: Option<ByteRange>,
    ) -> Result<ReadHandle, BackendError>;

    /// Writes the destination side of a client relay copy.
    async fn write_object(
        &self,
        dst: &ObjectRef,
        data: Vec<u8>,
        on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError>;

    /// The default implementation buffers into memory and calls `write_object`.
    async fn write_from_reader(
        &self,
        dst: &ObjectRef,
        mut reader: Pin<Box<dyn AsyncRead + Send>>,
        content_length: Option<u64>,
        on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        let expected_len = content_length
            .and_then(|len| usize::try_from(len).ok())
            .ok_or(BackendError::Unsupported {
                operation: WRITE_FROM_READER_MISSING_LENGTH_OPERATION,
            })?;
        let _permit = MemoryBudget::global().acquire(expected_len).await.ok_or(
            BackendError::Unsupported {
                operation: WRITE_FROM_READER_MEMORY_BUDGET_OPERATION,
            },
        )?;

        let mut data = Vec::with_capacity(expected_len);
        let mut buffer = [0_u8; WRITE_FROM_READER_BUFFER_BYTES];
        loop {
            let bytes_read = reader
                .read(&mut buffer)
                .await
                .map_err(|e| BackendError::Internal(Box::new(e)))?;
            if bytes_read == 0 {
                break;
            }
            let next_len = data
                .len()
                .checked_add(bytes_read)
                .ok_or(BackendError::Unsupported {
                    operation: WRITE_FROM_READER_LENGTH_EXCEEDED_OPERATION,
                })?;
            if next_len > expected_len {
                return Err(BackendError::Unsupported {
                    operation: WRITE_FROM_READER_LENGTH_EXCEEDED_OPERATION,
                });
            }
            data.extend_from_slice(&buffer[..bytes_read]);
        }
        self.write_object(dst, data, on_conflict).await
    }

    /// Used by resumable chunked transfers; backends without support return `Unsupported`.
    async fn write_range_from_reader(
        &self,
        _dst: &ObjectRef,
        _reader: Pin<Box<dyn AsyncRead + Send>>,
        _range: ByteRange,
        _final_size: u64,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported {
            operation: "write_range_from_reader",
        })
    }

    fn multipart_part_size(&self) -> Option<u64> {
        None
    }

    async fn create_multipart_upload(
        &self,
        _dst: &ObjectRef,
        _on_conflict: ConflictResolution,
    ) -> Result<MultipartUploadSession, BackendError> {
        Err(BackendError::Unsupported {
            operation: "create_multipart_upload",
        })
    }

    async fn list_multipart_parts(
        &self,
        _session: &MultipartUploadSession,
    ) -> Result<Vec<MultipartPart>, BackendError> {
        Err(BackendError::Unsupported {
            operation: "list_multipart_parts",
        })
    }

    async fn upload_multipart_part(
        &self,
        _session: &MultipartUploadSession,
        _part_number: i32,
        _data: Arc<[u8]>,
    ) -> Result<MultipartPart, BackendError> {
        Err(BackendError::Unsupported {
            operation: "upload_multipart_part",
        })
    }

    async fn complete_multipart_upload(
        &self,
        _session: &MultipartUploadSession,
        _parts: Vec<MultipartPart>,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported {
            operation: "complete_multipart_upload",
        })
    }

    async fn abort_multipart_upload(
        &self,
        _session: &MultipartUploadSession,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported {
            operation: "abort_multipart_upload",
        })
    }

    async fn mkdir(&self, parent: &ObjectRef, name: &str) -> Result<ObjectRef, BackendError>;

    /// Gives an object that a copy just created the source's modification time.
    ///
    /// Creating a file or directory always stamps the current time, so copies call this for that
    /// one value and leave every other piece of metadata alone. Implementations skip the update
    /// when the object already has `mtime`. Backends that cannot store a modification time (S3
    /// `LastModified` is the upload time) keep this default.
    async fn set_mtime(
        &self,
        _object: &ObjectRef,
        _mtime: std::time::SystemTime,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported {
            operation: "set_mtime",
        })
    }

    async fn copy(
        &self,
        src: &ObjectRef,
        dst: &ObjectRef,
        options: CopyOptions,
    ) -> Result<OperationTicket, BackendError>;

    async fn move_object(
        &self,
        src: &ObjectRef,
        dst: &ObjectRef,
        options: MoveOptions,
    ) -> Result<OperationTicket, BackendError>;

    async fn delete(
        &self,
        object: &ObjectRef,
        mode: DeleteMode,
    ) -> Result<OperationTicket, BackendError>;

    /// Thumbnails rendered by the storage service; most backends have none and keep this default.
    async fn get_thumbnail(
        &self,
        _object: &ObjectRef,
        _hint: ThumbnailHint,
    ) -> Result<ThumbnailResult, BackendError> {
        Err(BackendError::Unsupported {
            operation: "get_thumbnail",
        })
    }

    /// Metadata at the requested `level`. The default takes `stat` and records the level.
    async fn get_metadata(
        &self,
        object: &ObjectRef,
        level: MetadataLevel,
    ) -> Result<MetadataResult, BackendError> {
        let mut result = self.stat(object).await?;
        result.level = level;
        Ok(result)
    }

    async fn get_stable_id(&self, object: &ObjectRef) -> Result<StableId, BackendError>;

    fn capabilities(&self) -> CapabilitySet;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::object::{BackendId, ObjectKey};
    use crate::types::operation::{OperationKind, TicketId};
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct DefaultBufferingBackend {
        written: Arc<Mutex<Vec<u8>>>,
    }

    fn test_object(path: &str) -> ObjectRef {
        ObjectRef {
            key: ObjectKey {
                backend_id: BackendId("test".to_string()),
                stable_id: StableId(path.to_string()),
            },
            parent_stable_id: None,
            display_path: path.to_string(),
        }
    }

    fn unsupported<T>(operation: &'static str) -> Result<T, BackendError> {
        Err(BackendError::Unsupported { operation })
    }

    #[async_trait]
    impl BackendAdapter for DefaultBufferingBackend {
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
            unsupported("list_names")
        }

        async fn stat(&self, _object: &ObjectRef) -> Result<MetadataResult, BackendError> {
            unsupported("stat")
        }

        async fn read(
            &self,
            _object: &ObjectRef,
            _range: Option<ByteRange>,
        ) -> Result<ReadHandle, BackendError> {
            unsupported("read")
        }

        async fn write_object(
            &self,
            _dst: &ObjectRef,
            data: Vec<u8>,
            _on_conflict: ConflictResolution,
        ) -> Result<OperationTicket, BackendError> {
            *self.written.lock().expect("written lock") = data;
            Ok(OperationTicket {
                id: TicketId(1),
                kind: OperationKind::Copy,
            })
        }

        async fn mkdir(&self, _parent: &ObjectRef, _name: &str) -> Result<ObjectRef, BackendError> {
            unsupported("mkdir")
        }

        async fn copy(
            &self,
            _src: &ObjectRef,
            _dst: &ObjectRef,
            _options: CopyOptions,
        ) -> Result<OperationTicket, BackendError> {
            unsupported("copy")
        }

        async fn move_object(
            &self,
            _src: &ObjectRef,
            _dst: &ObjectRef,
            _options: MoveOptions,
        ) -> Result<OperationTicket, BackendError> {
            unsupported("move_object")
        }

        async fn delete(
            &self,
            _object: &ObjectRef,
            _mode: DeleteMode,
        ) -> Result<OperationTicket, BackendError> {
            unsupported("delete")
        }

        async fn get_metadata(
            &self,
            _object: &ObjectRef,
            _level: MetadataLevel,
        ) -> Result<MetadataResult, BackendError> {
            unsupported("get_metadata")
        }

        async fn get_stable_id(&self, _object: &ObjectRef) -> Result<StableId, BackendError> {
            unsupported("get_stable_id")
        }

        fn capabilities(&self) -> CapabilitySet {
            CapabilitySet::default()
        }
    }

    #[tokio::test]
    async fn default_write_from_reader_buffers_with_declared_length() {
        let backend = DefaultBufferingBackend::default();
        let payload = b"hello memory budget".to_vec();
        let ticket = backend
            .write_from_reader(
                &test_object("/dst"),
                Box::pin(Cursor::new(payload.clone())),
                Some(payload.len() as u64),
                ConflictResolution::Fail,
            )
            .await
            .expect("write_from_reader should succeed");

        assert!(matches!(ticket.kind, OperationKind::Copy));
        assert_eq!(*backend.written.lock().expect("written lock"), payload);
    }

    #[tokio::test]
    async fn default_write_from_reader_requires_content_length() {
        let backend = DefaultBufferingBackend::default();
        let error = backend
            .write_from_reader(
                &test_object("/dst"),
                Box::pin(Cursor::new(b"payload".to_vec())),
                None,
                ConflictResolution::Fail,
            )
            .await
            .expect_err("missing content length must fail");

        assert!(matches!(
            error,
            BackendError::Unsupported {
                operation: WRITE_FROM_READER_MISSING_LENGTH_OPERATION
            }
        ));
        assert!(backend.written.lock().expect("written lock").is_empty());
    }

    #[tokio::test]
    async fn default_write_from_reader_rejects_stream_longer_than_declared_length() {
        let backend = DefaultBufferingBackend::default();
        let error = backend
            .write_from_reader(
                &test_object("/dst"),
                Box::pin(Cursor::new(b"payload".to_vec())),
                Some(3),
                ConflictResolution::Fail,
            )
            .await
            .expect_err("overlong stream must fail");

        assert!(matches!(
            error,
            BackendError::Unsupported {
                operation: WRITE_FROM_READER_LENGTH_EXCEEDED_OPERATION
            }
        ));
        assert!(backend.written.lock().expect("written lock").is_empty());
    }
}
