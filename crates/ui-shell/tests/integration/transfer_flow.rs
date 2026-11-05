use async_trait::async_trait;
use iris_backends::localfs::adapter::LocalFsAdapter;
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::capability::CapabilitySet;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::list::{ListCursor, ListPage};
use iris_core_domain::types::metadata::{MetadataLevel, MetadataResult};
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_core_domain::types::operation::{OperationKind, OperationTicket, TicketId};
use iris_core_domain::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use iris_core_domain::types::progress::{ByteRange, ProgressInfo};
use iris_core_domain::types::read_handle::ReadHandle;
use iris_core_domain::types::session::BackendSession;
use iris_core_domain::types::thumbnail::{ThumbnailHint, ThumbnailResult};
use iris_transfer::executor::{client_relay_copy_with_options, CopyRelayOptions, TransferControl};
use std::pin::Pin;
use tempfile::tempdir;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::watch;
use tokio::time::{sleep, Duration};

fn make_ref(backend_id: &str, display_path: &str) -> ObjectRef {
    ObjectRef {
        key: ObjectKey {
            backend_id: BackendId(backend_id.to_string()),
            stable_id: StableId(display_path.to_string()),
        },
        parent_stable_id: None,
        display_path: display_path.to_string(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transfer_control_supports_pause_resume_and_cancel() {
    let src_root = tempdir().expect("src tempdir");
    let dst_root = tempdir().expect("dst tempdir");

    let src_file = src_root.path().join("from.bin");
    tokio::fs::write(&src_file, vec![0xAB; 256 * 1024])
        .await
        .expect("write source file");

    let src_backend = LocalFsAdapter::new(
        src_root.path().to_path_buf(),
        BackendId("local-src".to_string()),
    )
    .expect("create src backend");
    let dst_backend = LocalFsAdapter::new(
        dst_root.path().to_path_buf(),
        BackendId("local-dst".to_string()),
    )
    .expect("create dst backend");

    let src_ref = make_ref("local-src", "from.bin");
    let dst_ref = make_ref("local-dst", "to.bin");

    let (progress_tx, _progress_rx) = watch::channel(ProgressInfo {
        bytes_completed: 0,
        bytes_total: None,
        files_completed: 0,
        files_total: None,
    });

    let paused = TransferControl::new();
    paused.pause();
    let resume_after = paused.clone();
    tokio::spawn(async move {
        sleep(Duration::from_millis(5)).await;
        resume_after.resume();
    });

    let completed = client_relay_copy_with_options(
        &src_backend,
        &src_ref,
        &dst_backend,
        &dst_ref,
        &progress_tx,
        CopyRelayOptions {
            control: Some(&paused),
            ..Default::default()
        },
    )
    .await
    .expect("paused copy resumes");
    assert_eq!(completed.bytes_transferred, 256 * 1024);
    let copied = tokio::fs::read(dst_root.path().join("to.bin"))
        .await
        .expect("read destination");
    assert_eq!(copied.len(), 256 * 1024);

    let cancel_dst = make_ref("local-dst", "canceled.bin");
    let cancel_backend = SlowReadBackend::new(64 * 1024 * 1024);
    let slow_src = make_ref("slow-src", "slow.bin");
    let canceled = TransferControl::new();
    canceled.pause();
    let cancel_after = canceled.clone();
    tokio::spawn(async move {
        sleep(Duration::from_millis(5)).await;
        cancel_after.cancel();
    });

    let error = client_relay_copy_with_options(
        &cancel_backend,
        &slow_src,
        &dst_backend,
        &cancel_dst,
        &progress_tx,
        CopyRelayOptions {
            control: Some(&canceled),
            ..Default::default()
        },
    )
    .await
    .expect_err("canceled copy should fail");
    match error {
        BackendError::Internal(_) => {}
        other => panic!("unexpected cancel error: {other:?}"),
    }
}

struct SlowReadBackend {
    len: usize,
}

impl SlowReadBackend {
    fn new(len: usize) -> Self {
        Self { len }
    }
}

#[async_trait]
impl BackendAdapter for SlowReadBackend {
    async fn connect(&self) -> Result<BackendSession, BackendError> {
        Ok(BackendSession {
            backend_id: BackendId("slow-src".into()),
            display_name: "slow-src".into(),
        })
    }

    async fn list_names(
        &self,
        _dir: &ObjectRef,
        _cursor: Option<ListCursor>,
    ) -> Result<ListPage, BackendError> {
        Err(BackendError::Unsupported { operation: "list" })
    }

    async fn stat(&self, object: &ObjectRef) -> Result<MetadataResult, BackendError> {
        Ok(MetadataResult {
            level: MetadataLevel::L1,
            is_dir: false,
            size: Some(self.len as u64),
            mtime: None,
            ctime: None,
            content_type: Some("application/octet-stream".into()),
            etag: Some(format!("etag-{}", object.display_path)),
        })
    }

    async fn read(
        &self,
        _object: &ObjectRef,
        _range: Option<ByteRange>,
    ) -> Result<ReadHandle, BackendError> {
        let reader: Pin<Box<dyn AsyncRead + Send>> =
            Box::pin(tokio::io::repeat(0x42).take(self.len as u64));
        Ok(ReadHandle {
            stream: reader,
            content_length: Some(self.len as u64),
            content_range: None,
        })
    }

    async fn write_object(
        &self,
        _dst: &ObjectRef,
        _data: Vec<u8>,
        _on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported {
            operation: "write_object",
        })
    }

    async fn write_from_reader(
        &self,
        _dst: &ObjectRef,
        _reader: Pin<Box<dyn AsyncRead + Send>>,
        _content_length: Option<u64>,
        _on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        Err(BackendError::Unsupported {
            operation: "write_from_reader",
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
        Ok(OperationTicket {
            id: TicketId(99),
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
        object: &ObjectRef,
        level: MetadataLevel,
    ) -> Result<MetadataResult, BackendError> {
        Ok(MetadataResult {
            level,
            is_dir: false,
            size: Some(self.len as u64),
            mtime: None,
            ctime: None,
            content_type: Some("application/octet-stream".into()),
            etag: Some(format!("etag-{}", object.display_path)),
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
            can_provide_progress: true,
            can_verify_by_checksum: false,
            can_trash: false,
            can_etag: true,
        }
    }
}
