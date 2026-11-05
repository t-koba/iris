use async_trait::async_trait;
use criterion::{BenchmarkId, Criterion, Throughput};
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::capability::CapabilitySet;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::list::{ListCursor, ListPage};
use iris_core_domain::types::metadata::{MetadataLevel, MetadataResult};
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_core_domain::types::operation::{OperationKind, OperationTicket, TicketId};
use iris_core_domain::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use iris_core_domain::types::progress::ByteRange;
use iris_core_domain::types::progress::ProgressInfo;
use iris_core_domain::types::read_handle::ReadHandle;
use iris_core_domain::types::session::BackendSession;
use iris_core_domain::types::thumbnail::{ThumbnailHint, ThumbnailResult};
use iris_transfer::executor::client_relay_copy;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{duplex, AsyncWriteExt};
use tokio::runtime::Runtime;
use tokio::sync::watch;

#[derive(Default)]
struct MemoryStore {
    files: Mutex<HashMap<String, Vec<u8>>>,
}

pub struct MemoryBackend {
    backend_id: BackendId,
    caps: CapabilitySet,
    store: Arc<MemoryStore>,
}

impl MemoryBackend {
    pub fn new(backend_id: &str, caps: CapabilitySet) -> Self {
        Self {
            backend_id: BackendId(backend_id.to_string()),
            caps,
            store: Arc::new(MemoryStore::default()),
        }
    }

    pub fn put(&self, path: &str, bytes: Vec<u8>) {
        self.store
            .files
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(path.to_string(), bytes);
    }

    fn next_ticket(&self, kind: OperationKind) -> OperationTicket {
        OperationTicket {
            id: TicketId(1),
            kind,
        }
    }

    fn object_ref(&self, path: &str) -> ObjectRef {
        ObjectRef {
            key: ObjectKey {
                backend_id: self.backend_id.clone(),
                stable_id: StableId(path.to_string()),
            },
            parent_stable_id: None,
            display_path: path.to_string(),
        }
    }
}

#[async_trait]
impl BackendAdapter for MemoryBackend {
    async fn connect(&self) -> Result<BackendSession, BackendError> {
        Ok(BackendSession {
            backend_id: self.backend_id.clone(),
            display_name: self.backend_id.0.clone(),
        })
    }

    async fn list_names(
        &self,
        _dir: &ObjectRef,
        _cursor: Option<ListCursor>,
    ) -> Result<ListPage, BackendError> {
        Ok(ListPage {
            entries: Vec::new(),
            next_cursor: None,
        })
    }

    async fn stat(&self, object: &ObjectRef) -> Result<MetadataResult, BackendError> {
        let size = self
            .store
            .files
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&object.display_path)
            .map(|bytes| bytes.len() as u64);
        Ok(MetadataResult {
            level: MetadataLevel::L1,
            is_dir: false,
            size,
            mtime: None,
            ctime: None,
            content_type: Some("application/octet-stream".to_string()),
            etag: None,
        })
    }

    async fn read(
        &self,
        object: &ObjectRef,
        range: Option<ByteRange>,
    ) -> Result<ReadHandle, BackendError> {
        let bytes = self
            .store
            .files
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&object.display_path)
            .cloned()
            .ok_or_else(|| BackendError::NotFound {
                object: object.clone(),
            })?;
        let payload = match range {
            Some(ByteRange { offset, length }) => {
                let end = length
                    .map(|len| offset.saturating_add(len))
                    .unwrap_or(bytes.len() as u64)
                    .min(bytes.len() as u64) as usize;
                bytes[offset as usize..end].to_vec()
            }
            None => bytes,
        };
        let content_length = payload.len() as u64;
        let (mut tx, rx) = duplex(payload.len().max(1));
        tokio::spawn(async move {
            let _ = tx.write_all(&payload).await;
            let _ = tx.shutdown().await;
        });
        Ok(ReadHandle {
            stream: Box::pin(rx),
            content_length: Some(content_length),
            content_range: range,
        })
    }

    async fn write_object(
        &self,
        dst: &ObjectRef,
        data: Vec<u8>,
        _on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        self.store
            .files
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(dst.display_path.clone(), data);
        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn mkdir(&self, _parent: &ObjectRef, _name: &str) -> Result<ObjectRef, BackendError> {
        Err(BackendError::Unsupported {
            operation: "memory backend mkdir",
        })
    }

    async fn copy(
        &self,
        _src: &ObjectRef,
        _dst: &ObjectRef,
        _options: CopyOptions,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported {
            operation: "memory backend copy",
        })
    }

    async fn move_object(
        &self,
        _src: &ObjectRef,
        _dst: &ObjectRef,
        _options: MoveOptions,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported {
            operation: "memory backend move",
        })
    }

    async fn delete(
        &self,
        _object: &ObjectRef,
        _mode: DeleteMode,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported {
            operation: "memory backend delete",
        })
    }

    async fn get_thumbnail(
        &self,
        _object: &ObjectRef,
        _hint: ThumbnailHint,
    ) -> Result<ThumbnailResult, BackendError> {
        Err(BackendError::Unsupported {
            operation: "memory backend thumbnail",
        })
    }

    async fn get_metadata(
        &self,
        object: &ObjectRef,
        _level: MetadataLevel,
    ) -> Result<MetadataResult, BackendError> {
        self.stat(object).await
    }

    async fn get_stable_id(&self, object: &ObjectRef) -> Result<StableId, BackendError> {
        Ok(object.key.stable_id.clone())
    }

    fn capabilities(&self) -> CapabilitySet {
        self.caps.clone()
    }
}

pub fn bench_copy(c: &mut Criterion, bench_name: &str, backend_name: &str, size_bytes: usize) {
    let runtime = Runtime::new().expect("tokio runtime");
    let progress = watch::channel(ProgressInfo::single_file(0, Some(size_bytes as u64), false));
    let source_caps = CapabilitySet {
        can_range_read: true,
        can_range_write: false,
        can_provide_progress: true,
        can_verify_by_checksum: true,
        ..CapabilitySet::default()
    };
    let destination_caps = CapabilitySet {
        can_provide_progress: true,
        ..CapabilitySet::default()
    };
    let source = MemoryBackend::new(&format!("{backend_name}-src"), source_caps);
    let destination = MemoryBackend::new(&format!("{backend_name}-dst"), destination_caps);
    let src_ref = source.object_ref("/bench/source.bin");
    let dst_ref = destination.object_ref("/bench/destination.bin");
    source.put(&src_ref.display_path, vec![0x5A; size_bytes]);

    let mut group = c.benchmark_group(bench_name);
    group.throughput(Throughput::Bytes(size_bytes as u64));
    group.bench_with_input(
        BenchmarkId::new(backend_name, size_bytes),
        &size_bytes,
        |b, _| {
            b.iter(|| {
                runtime
                    .block_on(client_relay_copy(
                        &source,
                        &src_ref,
                        &destination,
                        &dst_ref,
                        &progress.0,
                    ))
                    .expect("relay copy");
            });
        },
    );
    group.finish();
}
