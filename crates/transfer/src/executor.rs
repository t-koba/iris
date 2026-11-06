use crate::chunked::{
    copy_chunked_to_multipart, copy_chunked_with_range_writes, read_chunked_to_vec,
    should_use_chunked_range, should_use_chunked_range_write, ChunkRetryControls,
    ChunkStateRecorder, ChunkedRangeWriteRequest, MultipartCopyRequest,
    CHUNKED_RANGE_DEFAULT_IN_FLIGHT, CHUNKED_RANGE_MIN_BYTES,
};
use crate::error_handler::RetryPolicy;
use crate::method_selector::recommended_workers_for_backend;
use crate::verify::verify_copy;
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::metadata::MetadataResult;
use iris_core_domain::types::object::ObjectRef;
use iris_core_domain::types::options::ConflictResolution;
use iris_core_domain::types::progress::ProgressInfo;
use iris_core_domain::types::transfer::JobId;
use iris_jobstore::repository::PersistedChunkState;
use iris_jobstore::writer_task::JobStore;
use std::future::Future;
use std::io::Cursor;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::watch;
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info_span, warn, Instrument};

type PauseFuture = Pin<Box<dyn Future<Output = Result<(), watch::error::RecvError>> + Send>>;

#[derive(Debug)]
pub struct TransferResult {
    pub bytes_transferred: u64,
}

struct ProgressReader {
    inner: Pin<Box<dyn AsyncRead + Send>>,
    progress_tx: watch::Sender<ProgressInfo>,
    bytes_read: u64,
    /// Bytes at the last notification; notify only after a large enough change.
    bytes_last_notified: u64,
    last_notify_at: Instant,
    bytes_total: Option<u64>,
    pause_rx: watch::Receiver<bool>,
    cancel_token: CancellationToken,
    wait_for_resume: Option<PauseFuture>,
}

const PROGRESS_NOTIFY_INTERVAL_SLOW: u64 = 64 * 1024;
const PROGRESS_NOTIFY_INTERVAL_DEFAULT: u64 = 256 * 1024;
const PROGRESS_NOTIFY_INTERVAL_FAST: u64 = 1024 * 1024;
const PROGRESS_NOTIFY_MIN_ELAPSED: Duration = Duration::from_millis(50);
const PROGRESS_NOTIFY_SLOW_BPS: u64 = 1024 * 1024;
const PROGRESS_NOTIFY_FAST_BPS: u64 = 50 * 1024 * 1024;

impl ProgressReader {
    fn new(
        inner: Pin<Box<dyn AsyncRead + Send>>,
        bytes_total: Option<u64>,
        progress_tx: watch::Sender<ProgressInfo>,
        pause_rx: watch::Receiver<bool>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            inner,
            progress_tx,
            bytes_read: 0,
            bytes_last_notified: 0,
            last_notify_at: Instant::now(),
            bytes_total,
            pause_rx,
            cancel_token,
            wait_for_resume: None,
        }
    }

    fn notify_interval_bytes(&self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.last_notify_at);
        if elapsed.is_zero() {
            return PROGRESS_NOTIFY_INTERVAL_DEFAULT;
        }

        let delta = self.bytes_read.saturating_sub(self.bytes_last_notified);
        let bytes_per_second = delta.saturating_mul(1000) / elapsed.as_millis().max(1) as u64;
        if bytes_per_second >= PROGRESS_NOTIFY_FAST_BPS {
            PROGRESS_NOTIFY_INTERVAL_FAST
        } else if bytes_per_second <= PROGRESS_NOTIFY_SLOW_BPS {
            PROGRESS_NOTIFY_INTERVAL_SLOW
        } else {
            PROGRESS_NOTIFY_INTERVAL_DEFAULT
        }
    }
}

impl AsyncRead for ProgressReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();

        if this.cancel_token.is_cancelled() {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "transfer canceled",
            )));
        }

        while *this.pause_rx.borrow() {
            if this.wait_for_resume.is_none() {
                let mut pause_rx = this.pause_rx.clone();
                this.wait_for_resume = Some(Box::pin(async move {
                    loop {
                        pause_rx.changed().await?;
                        if !*pause_rx.borrow() {
                            return Ok(());
                        }
                    }
                }));
            }

            match this
                .wait_for_resume
                .as_mut()
                .expect("pause future must exist")
                .as_mut()
                .poll(cx)
            {
                Poll::Ready(Ok(())) => {
                    this.wait_for_resume = None;
                }
                Poll::Ready(Err(_)) => {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "pause control channel closed",
                    )));
                }
                Poll::Pending => return Poll::Pending,
            }

            if this.cancel_token.is_cancelled() {
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "transfer canceled",
                )));
            }
        }

        let filled_before = buf.filled().len();

        match this.inner.as_mut().poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let bytes_just_read = (buf.filled().len() - filled_before) as u64;
                if bytes_just_read > 0 {
                    this.bytes_read += bytes_just_read;
                    let now = Instant::now();
                    let delta = this.bytes_read - this.bytes_last_notified;
                    let is_eof = bytes_just_read == 0
                        || this.bytes_total.is_some_and(|t| this.bytes_read >= t);
                    let interval = this.notify_interval_bytes(now);
                    let elapsed = now.saturating_duration_since(this.last_notify_at);
                    if delta >= interval || elapsed >= PROGRESS_NOTIFY_MIN_ELAPSED || is_eof {
                        this.bytes_last_notified = this.bytes_read;
                        this.last_notify_at = now;
                        let _ = this.progress_tx.send(ProgressInfo::single_file(
                            this.bytes_read,
                            this.bytes_total,
                            false,
                        ));
                    }
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TransferControl {
    pause_tx: watch::Sender<bool>,
    cancel_token: CancellationToken,
}

impl TransferControl {
    pub fn new() -> Self {
        let (pause_tx, _pause_rx) = watch::channel(false);
        Self {
            pause_tx,
            cancel_token: CancellationToken::new(),
        }
    }

    /// `send_replace`, not `send`: with no transfer subscribed at the moment, `send` would drop
    /// the request and the next file of the job would start unpaused.
    pub fn pause(&self) {
        self.pause_tx.send_replace(true);
    }

    pub fn resume(&self) {
        self.pause_tx.send_replace(false);
    }

    pub fn cancel(&self) {
        self.cancel_token.cancel();
    }

    pub fn pause_receiver(&self) -> watch::Receiver<bool> {
        self.pause_tx.subscribe()
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }
}

impl Default for TransferControl {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Default)]
pub struct MoveRelayOptions<'a> {
    pub control: Option<&'a TransferControl>,
    pub resumable: Option<&'a ResumableTransferContext>,
    pub user_requested_strong_verify: bool,
    pub high_risk_path: bool,
    pub retry_policy: Option<&'a RetryPolicy>,
}

#[derive(Clone)]
pub struct ResumableTransferContext {
    pub job_store: Arc<JobStore>,
    pub job_id: JobId,
    pub entry_index: u64,
}

#[derive(Clone, Copy, Default)]
pub struct CopyRelayOptions<'a> {
    pub control: Option<&'a TransferControl>,
    pub resumable: Option<&'a ResumableTransferContext>,
    pub retry_policy: Option<&'a RetryPolicy>,
}

fn source_resume_identity(metadata: &MetadataResult) -> Option<String> {
    if let Some(etag) = metadata.etag.as_deref() {
        return Some(format!("etag:{etag}"));
    }
    let modified = metadata
        .mtime?
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()?;
    Some(format!(
        "mtime:{}:{}:size:{}",
        modified.as_secs(),
        modified.subsec_nanos(),
        metadata.size?
    ))
}

#[tracing::instrument(skip(src_backend, dst_backend, progress_tx, options), fields(src = %src.display_path, dst = %dst.display_path))]
/// Reads `src` and writes `dst`, reporting byte progress on the watch channel.
pub async fn client_relay_copy(
    src_backend: &dyn BackendAdapter,
    src: &ObjectRef,
    dst_backend: &dyn BackendAdapter,
    dst: &ObjectRef,
    progress_tx: &watch::Sender<ProgressInfo>,
    options: CopyRelayOptions<'_>,
) -> Result<TransferResult, BackendError> {
    let src_capabilities = src_backend.capabilities();
    // Range reads need the size up front; otherwise the stat only feeds the mtime and is optional.
    let src_metadata = match src_backend.stat(src).await {
        Ok(metadata) => Some(metadata),
        Err(error) if src_capabilities.can_range_read => return Err(error),
        Err(_) => None,
    };
    let result = relay_copy_data(
        src_backend,
        src,
        dst_backend,
        dst,
        progress_tx,
        options,
        src_metadata
            .as_ref()
            .filter(|_| src_capabilities.can_range_read),
    )
    .await?;
    if let Some(mtime) = src_metadata.and_then(|metadata| metadata.mtime) {
        preserve_destination_mtime(dst_backend, dst, mtime).await;
    }
    Ok(result)
}

/// The relay wrote a new object, which always carries the time of the write; give it the source's
/// modification time. Destinations that cannot store one (S3, pCloud) are left as they are, and a
/// failure only loses the timestamp, never the copied bytes, so it is logged rather than returned.
async fn preserve_destination_mtime(
    dst_backend: &dyn BackendAdapter,
    dst: &ObjectRef,
    mtime: std::time::SystemTime,
) {
    match dst_backend.set_mtime(dst, mtime).await {
        Ok(()) | Err(BackendError::Unsupported { .. }) => {}
        Err(error) => warn!(
            dst_path_hash = %iris_core_domain::observability::path_hash(&dst.display_path),
            "failed to preserve the source modification time: {error}"
        ),
    }
}

async fn relay_copy_data(
    src_backend: &dyn BackendAdapter,
    src: &ObjectRef,
    dst_backend: &dyn BackendAdapter,
    dst: &ObjectRef,
    progress_tx: &watch::Sender<ProgressInfo>,
    options: CopyRelayOptions<'_>,
    range_metadata: Option<&MetadataResult>,
) -> Result<TransferResult, BackendError> {
    let control = options.control.cloned().unwrap_or_default();
    let retry_policy = options.retry_policy.cloned().unwrap_or_default();
    let src_capabilities = src_backend.capabilities();
    let dst_capabilities = dst_backend.capabilities();
    let range_total_bytes =
        range_metadata.and_then(|metadata| (!metadata.is_dir).then_some(metadata.size).flatten());
    if let (Some(context), Some(part_size), Some(total_len)) = (
        options.resumable,
        dst_backend.multipart_part_size(),
        range_total_bytes,
    ) {
        if src_capabilities.can_range_read && total_len >= CHUNKED_RANGE_MIN_BYTES {
            let max_in_flight = recommended_workers_for_backend(
                &dst.key.backend_id.0,
                CHUNKED_RANGE_DEFAULT_IN_FLIGHT,
            );
            copy_chunked_to_multipart(MultipartCopyRequest {
                src_backend,
                src: src.clone(),
                dst_backend,
                dst,
                total_len,
                part_size,
                source_identity: range_metadata.and_then(source_resume_identity),
                requested_in_flight: max_in_flight,
                job_store: Arc::clone(&context.job_store),
                job_id: context.job_id.clone(),
                entry_index: context.entry_index,
                progress_tx,
                retry: ChunkRetryControls {
                    policy: retry_policy,
                    pause_rx: control.pause_receiver(),
                    cancel_token: control.cancel_token(),
                },
            })
            .await?;
            return Ok(finish_file(progress_tx, total_len));
        }
    }
    let chunked_in_flight =
        || recommended_workers_for_backend(&src.key.backend_id.0, CHUNKED_RANGE_DEFAULT_IN_FLIGHT);
    let new_recorder = || {
        options.resumable.map(|context| {
            ChunkStateRecorder::new(
                Arc::clone(&context.job_store),
                context.job_id.clone(),
                context.entry_index,
                range_metadata.and_then(|metadata| metadata.etag.clone()),
            )
        })
    };
    let mut chunk_recording = None;
    let (source_reader, total_bytes): (Pin<Box<dyn AsyncRead + Send>>, Option<u64>) =
        if should_use_chunked_range_write(
            range_total_bytes,
            src_capabilities.can_range_read,
            dst_capabilities.can_range_write,
        ) {
            let total_bytes =
                range_total_bytes.expect("chunked range write copy requires known size");
            let max_in_flight = chunked_in_flight();
            let recorder = new_recorder();
            copy_chunked_with_range_writes(ChunkedRangeWriteRequest {
                src_backend,
                src: src.clone(),
                dst_backend,
                dst,
                total_len: total_bytes,
                requested_in_flight: max_in_flight,
                recorder,
                progress_tx,
                pause_rx: control.pause_receiver(),
                cancel_token: control.cancel_token(),
                retry_policy: retry_policy.clone(),
            })
            .await?;
            return Ok(finish_file(progress_tx, total_bytes));
        } else if should_use_chunked_range(range_total_bytes, src_capabilities.can_range_read) {
            let total_bytes = range_total_bytes.expect("chunked range copy requires known size");
            let max_in_flight = chunked_in_flight();
            let recorder = new_recorder();
            chunk_recording = recorder
                .as_ref()
                .map(|recorder| (recorder.clone(), total_bytes));
            let bytes = read_chunked_to_vec(
                src_backend,
                src.clone(),
                total_bytes,
                max_in_flight,
                recorder,
                ChunkRetryControls {
                    policy: retry_policy,
                    pause_rx: control.pause_receiver(),
                    cancel_token: control.cancel_token(),
                },
            )
            .await?;
            (Box::pin(Cursor::new(bytes)), Some(total_bytes))
        } else {
            let src_handle = src_backend.read(src, None).await?;
            (src_handle.stream, src_handle.content_length)
        };
    let reader = ProgressReader::new(
        source_reader,
        total_bytes,
        progress_tx.clone(),
        control.pause_receiver(),
        control.cancel_token(),
    );

    let write_result = dst_backend
        .write_from_reader(dst, Box::pin(reader), total_bytes, ConflictResolution::Fail)
        .await;
    if let Some((recorder, total_len)) = &chunk_recording {
        recorder.record_transfer_outcome(
            *total_len,
            if write_result.is_ok() {
                PersistedChunkState::Completed
            } else {
                PersistedChunkState::Failed
            },
        )?;
    }
    write_result?;

    let bytes_read = progress_tx.borrow().bytes_completed;

    // Batched notifications may have skipped the final value, so send it on completion.
    if let Some(total) = total_bytes {
        let _ = progress_tx.send(ProgressInfo::single_file(total, Some(total), true));
    }

    debug!("client relay copy completed: {} bytes", bytes_read);

    Ok(TransferResult {
        bytes_transferred: total_bytes.unwrap_or(bytes_read),
    })
}

/// Reports a single-file transfer of `bytes` as complete.
pub fn finish_file(progress_tx: &watch::Sender<ProgressInfo>, bytes: u64) -> TransferResult {
    let _ = progress_tx.send(ProgressInfo::single_file(bytes, Some(bytes), true));
    TransferResult {
        bytes_transferred: bytes,
    }
}

/// Only called after verification succeeded.
#[tracing::instrument(
    skip(src_backend),
    fields(path_hash = %iris_core_domain::observability::path_hash(&src.display_path))
)]
pub async fn delete_source_after_verify(
    src_backend: &dyn BackendAdapter,
    src: &ObjectRef,
) -> Result<(), BackendError> {
    use iris_core_domain::types::options::DeleteMode;

    debug!("deleting source after successful verify");
    src_backend.delete(src, DeleteMode::Permanent).await?;
    Ok(())
}

/// Standard cross-backend move: copy, then verify, then delete the source.
#[tracing::instrument(
    skip(src_backend, dst_backend, progress_tx, options),
    fields(
        src_path_hash = %iris_core_domain::observability::path_hash(&src.display_path),
        dst_path_hash = %iris_core_domain::observability::path_hash(&dst.display_path)
    )
)]
pub async fn move_via_relay_with_verify_and_control(
    src_backend: &dyn BackendAdapter,
    src: &ObjectRef,
    dst_backend: &dyn BackendAdapter,
    dst: &ObjectRef,
    progress_tx: &watch::Sender<ProgressInfo>,
    options: MoveRelayOptions<'_>,
) -> Result<TransferResult, BackendError> {
    let MoveRelayOptions {
        control,
        resumable,
        user_requested_strong_verify,
        high_risk_path,
        retry_policy,
    } = options;
    let copy_span = info_span!("transfer.copy_relay");
    let copy_result = async {
        client_relay_copy(
            src_backend,
            src,
            dst_backend,
            dst,
            progress_tx,
            CopyRelayOptions {
                control,
                resumable,
                retry_policy,
            },
        )
        .await
    }
    .instrument(copy_span)
    .await?;

    let verify_span = info_span!("transfer.verify");
    let verdict = async {
        verify_copy(
            src_backend,
            src,
            dst_backend,
            dst,
            user_requested_strong_verify,
            high_risk_path,
        )
        .await
    }
    .instrument(verify_span)
    .await?;
    if let Err(reason) = verdict {
        return Err(BackendError::Internal(Box::new(std::io::Error::other(
            reason,
        ))));
    }

    delete_source_after_verify(src_backend, src).await?;
    Ok(copy_result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use iris_backends::localfs::adapter::LocalFsAdapter;
    use iris_core_domain::types::capability::CapabilitySet;
    use iris_core_domain::types::error::{BackendError, NetworkErrorDetail};
    use iris_core_domain::types::metadata::{MetadataLevel, MetadataResult};
    use iris_core_domain::types::multipart::{MultipartPart, MultipartUploadSession};
    use iris_core_domain::types::object::{BackendId, ObjectKey, StableId};
    use iris_core_domain::types::operation::{OperationKind, OperationTicket, TicketId};
    use iris_core_domain::types::options::{CopyOptions, DeleteMode, MoveOptions};
    use iris_core_domain::types::progress::ByteRange;
    use iris_core_domain::types::read_handle::ReadHandle;
    use iris_core_domain::types::session::BackendSession;

    use iris_core_domain::types::transfer::{JobState, TaskId, TaskKind, TaskState};
    use iris_jobstore::repository::{
        PersistedChunkState, PersistedChunkStateRow, PersistedJob, PersistedTask,
    };
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;
    use tokio::fs;
    use tokio::io::AsyncReadExt;

    fn make_ref(backend_id: &str, display_path: &str) -> ObjectRef {
        ObjectRef {
            key: ObjectKey {
                backend_id: BackendId(backend_id.to_string()),
                stable_id: StableId("test".to_string()),
            },
            parent_stable_id: None,
            display_path: display_path.to_string(),
        }
    }

    #[tokio::test]
    async fn client_relay_copy_writes_destination_content() {
        let src_root = tempdir().expect("create src tempdir");
        let dst_root = tempdir().expect("create dst tempdir");

        let src_file = src_root.path().join("from.txt");
        fs::write(&src_file, b"relay-copy-regression")
            .await
            .expect("write source file");

        let src_backend = LocalFsAdapter::new(
            src_root.path().to_path_buf(),
            BackendId("local-src".to_string()),
        );
        let dst_backend = LocalFsAdapter::new(
            dst_root.path().to_path_buf(),
            BackendId("local-dst".to_string()),
        );

        let src_ref = make_ref("local-src", "from.txt");
        let dst_ref = make_ref("local-dst", "to.txt");

        let (tx, _rx) = watch::channel(ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: None,
        });

        let result = client_relay_copy(
            &src_backend,
            &src_ref,
            &dst_backend,
            &dst_ref,
            &tx,
            CopyRelayOptions::default(),
        )
        .await
        .expect("relay copy should succeed");

        assert_eq!(result.bytes_transferred, 21);

        let copied = fs::read(dst_root.path().join("to.txt"))
            .await
            .expect("read copied file");
        assert_eq!(copied, b"relay-copy-regression");
    }

    fn set_file_mtime(path: &std::path::Path, mtime: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open for times")
            .set_modified(mtime)
            .expect("set mtime");
    }

    fn file_mtime(path: &std::path::Path) -> std::time::SystemTime {
        std::fs::metadata(path)
            .expect("metadata")
            .modified()
            .expect("mtime")
    }

    #[tokio::test]
    async fn client_relay_copy_gives_new_destination_the_source_mtime() {
        let src_root = tempdir().expect("create src tempdir");
        let dst_root = tempdir().expect("create dst tempdir");
        let src_file = src_root.path().join("photo.jpg");
        fs::write(&src_file, b"jpeg bytes")
            .await
            .expect("write source");
        let source_mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000);
        set_file_mtime(&src_file, source_mtime);

        let src_backend =
            LocalFsAdapter::new(src_root.path().to_path_buf(), BackendId("local-src".into()));
        let dst_backend =
            LocalFsAdapter::new(dst_root.path().to_path_buf(), BackendId("local-dst".into()));
        let (tx, _rx) = watch::channel(ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: None,
        });

        client_relay_copy(
            &src_backend,
            &make_ref("local-src", "photo.jpg"),
            &dst_backend,
            &make_ref("local-dst", "photo.jpg"),
            &tx,
            CopyRelayOptions::default(),
        )
        .await
        .expect("relay copy");

        assert_eq!(file_mtime(&dst_root.path().join("photo.jpg")), source_mtime);
        assert_eq!(
            file_mtime(&src_file),
            source_mtime,
            "source is never touched"
        );
    }

    #[tokio::test]
    async fn relay_move_between_file_systems_verifies_and_deletes_source() {
        // Cross-device moves used to fail VerifyLight: the copy carried the write time.
        let src_root = tempdir().expect("create src tempdir");
        let dst_root = tempdir().expect("create dst tempdir");
        let src_file = src_root.path().join("clip.mov");
        fs::write(&src_file, vec![7u8; 4096])
            .await
            .expect("write source");
        let source_mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_400_000_000);
        set_file_mtime(&src_file, source_mtime);

        let src_backend =
            LocalFsAdapter::new(src_root.path().to_path_buf(), BackendId("local-src".into()));
        let dst_backend =
            LocalFsAdapter::new(dst_root.path().to_path_buf(), BackendId("local-dst".into()));
        let (tx, _rx) = watch::channel(ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: None,
        });

        move_via_relay_with_verify_and_control(
            &src_backend,
            &make_ref("local-src", "clip.mov"),
            &dst_backend,
            &make_ref("local-dst", "clip.mov"),
            &tx,
            MoveRelayOptions {
                high_risk_path: true,
                ..MoveRelayOptions::default()
            },
        )
        .await
        .expect("relay move");

        let dst_file = dst_root.path().join("clip.mov");
        assert_eq!(
            fs::read(&dst_file).await.expect("read dst"),
            vec![7u8; 4096]
        );
        assert_eq!(file_mtime(&dst_file), source_mtime);
        assert!(!src_file.exists(), "source is deleted after verification");
    }

    #[tokio::test]
    async fn pause_requested_before_any_transfer_subscribes_still_applies() {
        let control = TransferControl::new();
        control.pause();

        assert!(*control.pause_receiver().borrow());
        control.resume();
        assert!(!*control.pause_receiver().borrow());
    }

    type MockMultipartUploads = Arc<Mutex<HashMap<String, (String, HashMap<i32, Vec<u8>>)>>>;

    struct MockBackend {
        backend_id: String,
        files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        deleted: Arc<Mutex<Vec<String>>>,
        read_ranges: Arc<Mutex<Vec<Option<ByteRange>>>>,
        stream_writes: Arc<Mutex<u32>>,
        range_writes: Arc<AtomicU32>,
        read_failures_remaining: Arc<AtomicU32>,
        short_reads_remaining: Arc<AtomicU32>,
        range_write_failures_remaining: Arc<AtomicU32>,
        multipart_uploads: MockMultipartUploads,
        multipart_next_id: Arc<AtomicU32>,
        multipart_successes_remaining: Arc<AtomicU32>,
        multipart_competing_data: Option<Vec<u8>>,
        multipart_abort_calls: Arc<AtomicU32>,
        multipart_abort_failures_remaining: Arc<AtomicU32>,
        multipart_list_failures_remaining: Arc<AtomicU32>,
        multipart_complete_failures_remaining: Arc<AtomicU32>,
        multipart_interrupt_after_abort: Arc<AtomicU32>,
        multipart_interrupt_after_completion: bool,
        multipart_interrupted: tokio::sync::Notify,
        corrupt_writes: bool,
    }

    impl MockBackend {
        fn new(backend_id: &str, corrupt_writes: bool) -> Self {
            Self {
                backend_id: backend_id.to_string(),
                files: Arc::new(Mutex::new(HashMap::new())),
                deleted: Arc::new(Mutex::new(Vec::new())),
                read_ranges: Arc::new(Mutex::new(Vec::new())),
                stream_writes: Arc::new(Mutex::new(0)),
                range_writes: Arc::new(AtomicU32::new(0)),
                read_failures_remaining: Arc::new(AtomicU32::new(0)),
                short_reads_remaining: Arc::new(AtomicU32::new(0)),
                range_write_failures_remaining: Arc::new(AtomicU32::new(0)),
                multipart_uploads: Arc::new(Mutex::new(HashMap::new())),
                multipart_next_id: Arc::new(AtomicU32::new(1)),
                multipart_successes_remaining: Arc::new(AtomicU32::new(u32::MAX)),
                multipart_competing_data: None,
                multipart_abort_calls: Arc::new(AtomicU32::new(0)),
                multipart_abort_failures_remaining: Arc::new(AtomicU32::new(0)),
                multipart_list_failures_remaining: Arc::new(AtomicU32::new(0)),
                multipart_complete_failures_remaining: Arc::new(AtomicU32::new(0)),
                multipart_interrupt_after_abort: Arc::new(AtomicU32::new(0)),
                multipart_interrupt_after_completion: false,
                multipart_interrupted: tokio::sync::Notify::new(),
                corrupt_writes,
            }
        }

        fn put(&self, path: &str, bytes: &[u8]) {
            self.files
                .lock()
                .expect("files lock")
                .insert(path.to_string(), bytes.to_vec());
        }

        fn get(&self, path: &str) -> Option<Vec<u8>> {
            self.files.lock().expect("files lock").get(path).cloned()
        }

        fn deleted_paths(&self) -> Vec<String> {
            self.deleted.lock().expect("deleted lock").clone()
        }

        fn read_ranges(&self) -> Vec<Option<ByteRange>> {
            self.read_ranges.lock().expect("read_ranges lock").clone()
        }

        fn stream_write_count(&self) -> u32 {
            *self.stream_writes.lock().expect("stream_writes lock")
        }

        fn fail_next_reads(&self, count: u32) {
            self.read_failures_remaining.store(count, Ordering::Relaxed);
        }

        fn fail_next_range_writes(&self, count: u32) {
            self.range_write_failures_remaining
                .store(count, Ordering::Relaxed);
        }

        fn short_next_reads(&self, count: u32) {
            self.short_reads_remaining.store(count, Ordering::Relaxed);
        }

        fn range_write_count(&self) -> u32 {
            self.range_writes.load(Ordering::Relaxed)
        }

        fn fail_multipart_after_successes(&self, count: u32) {
            self.multipart_successes_remaining
                .store(count, Ordering::Relaxed);
        }

        fn fail_next_aborts(&self, count: u32) {
            self.multipart_abort_failures_remaining
                .store(count, Ordering::Relaxed);
        }

        fn fail_next_lists(&self, count: u32) {
            self.multipart_list_failures_remaining
                .store(count, Ordering::Relaxed);
        }

        fn fail_next_completes(&self, count: u32) {
            self.multipart_complete_failures_remaining
                .store(count, Ordering::Relaxed);
        }

        fn abort_call_count(&self) -> u32 {
            self.multipart_abort_calls.load(Ordering::Relaxed)
        }

        fn created_multipart_uploads(&self) -> u32 {
            self.multipart_next_id.load(Ordering::Relaxed) - 1
        }
    }

    #[async_trait]
    impl BackendAdapter for MockBackend {
        async fn connect(&self) -> Result<BackendSession, BackendError> {
            Ok(BackendSession {
                backend_id: BackendId(self.backend_id.clone()),
                display_name: self.backend_id.clone(),
            })
        }

        async fn list_names(
            &self,
            _dir: &ObjectRef,
            _cursor: Option<iris_core_domain::types::list::ListCursor>,
        ) -> Result<iris_core_domain::types::list::ListPage, BackendError> {
            Err(BackendError::Unsupported { operation: "list" })
        }

        async fn stat(&self, object: &ObjectRef) -> Result<MetadataResult, BackendError> {
            self.get_metadata(object, MetadataLevel::L1).await
        }

        async fn read(
            &self,
            object: &ObjectRef,
            range: Option<ByteRange>,
        ) -> Result<ReadHandle, BackendError> {
            self.read_ranges
                .lock()
                .expect("read_ranges lock")
                .push(range.clone());
            if consume_failure(&self.read_failures_remaining) {
                return Err(injected_network_error("range read"));
            }
            let data = self
                .files
                .lock()
                .expect("files lock")
                .get(&object.display_path)
                .cloned()
                .unwrap_or_default();
            let mut payload = match &range {
                Some(range) => {
                    let start = usize::try_from(range.offset)
                        .unwrap_or(data.len())
                        .min(data.len());
                    let end = match range.length {
                        Some(length) => start
                            .saturating_add(usize::try_from(length).unwrap_or(usize::MAX))
                            .min(data.len()),
                        None => data.len(),
                    };
                    data[start..end].to_vec()
                }
                None => data,
            };
            let mut len = payload.len() as u64;
            if consume_failure(&self.short_reads_remaining) {
                payload.truncate(payload.len() / 2);
                // Whole-stream failures retain the advertised original length. Keep the existing
                // range-read fixture semantics for chunk retry tests.
                if range.is_some() {
                    len = payload.len() as u64;
                }
            }
            Ok(ReadHandle {
                stream: Box::pin(Cursor::new(payload)),
                content_length: Some(len),
                content_range: range,
            })
        }

        async fn write_object(
            &self,
            dst: &ObjectRef,
            data: Vec<u8>,
            _on_conflict: ConflictResolution,
        ) -> Result<OperationTicket, BackendError> {
            let bytes = if self.corrupt_writes {
                data.into_iter().take(1).collect()
            } else {
                data
            };
            self.files
                .lock()
                .expect("files lock")
                .insert(dst.display_path.clone(), bytes);
            Ok(OperationTicket {
                id: TicketId(1),
                kind: OperationKind::Copy,
            })
        }

        async fn write_from_reader(
            &self,
            dst: &ObjectRef,
            mut reader: Pin<Box<dyn AsyncRead + Send>>,
            _content_length: Option<u64>,
            _on_conflict: ConflictResolution,
        ) -> Result<OperationTicket, BackendError> {
            let mut data = Vec::new();
            reader
                .read_to_end(&mut data)
                .await
                .map_err(|e| BackendError::Internal(Box::new(e)))?;
            let bytes = if self.corrupt_writes {
                data.into_iter().take(1).collect()
            } else {
                data
            };
            self.files
                .lock()
                .expect("files lock")
                .insert(dst.display_path.clone(), bytes);
            *self.stream_writes.lock().expect("stream_writes lock") += 1;
            Ok(OperationTicket {
                id: TicketId(1),
                kind: OperationKind::Copy,
            })
        }

        async fn write_range_from_reader(
            &self,
            dst: &ObjectRef,
            mut reader: Pin<Box<dyn AsyncRead + Send>>,
            range: ByteRange,
            final_size: u64,
        ) -> Result<OperationTicket, BackendError> {
            self.range_writes.fetch_add(1, Ordering::Relaxed);
            if consume_failure(&self.range_write_failures_remaining) {
                return Err(injected_network_error("range write"));
            }
            let expected_len = range
                .length
                .unwrap_or_else(|| final_size.saturating_sub(range.offset));
            let mut data = Vec::new();
            reader
                .read_to_end(&mut data)
                .await
                .map_err(|e| BackendError::Internal(Box::new(e)))?;
            let bytes = if self.corrupt_writes {
                data.into_iter().take(1).collect::<Vec<_>>()
            } else {
                data
            };
            if bytes.len() as u64 != expected_len {
                return Err(BackendError::Internal(Box::new(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!(
                        "range write received {} bytes for expected {expected_len}",
                        bytes.len()
                    ),
                ))));
            }

            let final_size = usize::try_from(final_size).map_err(|_| {
                BackendError::Internal(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "final range-write size does not fit usize",
                )))
            })?;
            let start = usize::try_from(range.offset).map_err(|_| {
                BackendError::Internal(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "range offset does not fit usize",
                )))
            })?;
            let end = start.checked_add(bytes.len()).ok_or_else(|| {
                BackendError::Internal(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "range write end offset overflowed",
                )))
            })?;
            if end > final_size {
                return Err(BackendError::Internal(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "range write exceeds final object size",
                ))));
            }

            let mut files = self.files.lock().expect("files lock");
            let file = files
                .entry(dst.display_path.clone())
                .or_insert_with(|| vec![0; final_size]);
            if file.len() != final_size {
                file.resize(final_size, 0);
            }
            file[start..end].copy_from_slice(&bytes);
            Ok(OperationTicket {
                id: TicketId(1),
                kind: OperationKind::Copy,
            })
        }

        fn multipart_part_size(&self) -> Option<u64> {
            self.backend_id
                .starts_with("s3-multipart")
                .then_some(crate::chunked::CHUNKED_RANGE_CHUNK_BYTES)
        }

        async fn create_multipart_upload(
            &self,
            dst: &ObjectRef,
            _on_conflict: ConflictResolution,
        ) -> Result<MultipartUploadSession, BackendError> {
            let id = format!(
                "upload-{}",
                self.multipart_next_id.fetch_add(1, Ordering::Relaxed)
            );
            self.multipart_uploads
                .lock()
                .expect("multipart uploads lock")
                .insert(id.clone(), (dst.display_path.clone(), HashMap::new()));
            Ok(MultipartUploadSession {
                upload_id: id.clone(),
                destination: ObjectRef {
                    key: ObjectKey {
                        backend_id: BackendId(self.backend_id.clone()),
                        stable_id: StableId(id),
                    },
                    parent_stable_id: dst.parent_stable_id.clone(),
                    display_path: dst.display_path.clone(),
                },
            })
        }

        async fn list_multipart_parts(
            &self,
            session: &MultipartUploadSession,
        ) -> Result<Vec<MultipartPart>, BackendError> {
            if consume_failure(&self.multipart_list_failures_remaining) {
                return Err(injected_network_error("multipart list"));
            }
            let uploads = self
                .multipart_uploads
                .lock()
                .expect("multipart uploads lock");
            let (_, parts) =
                uploads
                    .get(&session.upload_id)
                    .ok_or_else(|| BackendError::NotFound {
                        object: session.destination.clone(),
                    })?;
            let mut result = parts
                .iter()
                .map(|(part_number, data)| MultipartPart {
                    part_number: *part_number,
                    etag: format!("part-etag-{part_number}"),
                    size: data.len() as u64,
                })
                .collect::<Vec<_>>();
            result.sort_by_key(|part| part.part_number);
            Ok(result)
        }

        async fn upload_multipart_part(
            &self,
            session: &MultipartUploadSession,
            part_number: i32,
            data: Arc<[u8]>,
        ) -> Result<MultipartPart, BackendError> {
            let allowed = self.multipart_successes_remaining.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |remaining| {
                    if remaining == u32::MAX {
                        None
                    } else {
                        remaining.checked_sub(1)
                    }
                },
            );
            if self.multipart_successes_remaining.load(Ordering::Relaxed) == 0 && allowed.is_err() {
                return Err(BackendError::Internal(Box::new(std::io::Error::other(
                    "injected multipart interruption",
                ))));
            }
            let size = data.len() as u64;
            let mut uploads = self
                .multipart_uploads
                .lock()
                .expect("multipart uploads lock");
            let (_, parts) = uploads.get_mut(&session.upload_id).ok_or_else(|| {
                BackendError::Internal(Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "multipart upload not found",
                )))
            })?;
            parts.insert(part_number, data.to_vec());
            Ok(MultipartPart {
                part_number,
                etag: format!("part-etag-{part_number}"),
                size,
            })
        }

        async fn complete_multipart_upload(
            &self,
            session: &MultipartUploadSession,
            parts: Vec<MultipartPart>,
            on_conflict: ConflictResolution,
        ) -> Result<OperationTicket, BackendError> {
            if consume_failure(&self.multipart_complete_failures_remaining) {
                return Err(injected_network_error("multipart complete"));
            }
            let (_, uploaded) = self
                .multipart_uploads
                .lock()
                .expect("multipart uploads lock")
                .get(&session.upload_id)
                .cloned()
                .ok_or_else(|| {
                    BackendError::Internal(Box::new(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "multipart upload not found",
                    )))
                })?;
            let mut data = Vec::new();
            for part in parts {
                data.extend_from_slice(uploaded.get(&part.part_number).ok_or_else(|| {
                    BackendError::Internal(Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "multipart part missing",
                    )))
                })?);
            }
            if let Some(competing) = &self.multipart_competing_data {
                // Another creator wins just before conditional completion.
                self.put(&session.destination.display_path, competing);
            }
            if !matches!(on_conflict, ConflictResolution::Overwrite)
                && self.get(&session.destination.display_path).is_some()
            {
                return Err(BackendError::Conflict {
                    object: session.destination.clone(),
                });
            }
            self.files
                .lock()
                .expect("files lock")
                .insert(session.destination.display_path.clone(), data);
            self.multipart_uploads
                .lock()
                .unwrap()
                .remove(&session.upload_id);
            if self.multipart_interrupt_after_completion {
                self.multipart_interrupted.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(OperationTicket {
                id: TicketId(1),
                kind: OperationKind::Copy,
            })
        }

        async fn abort_multipart_upload(
            &self,
            session: &MultipartUploadSession,
        ) -> Result<(), BackendError> {
            self.multipart_abort_calls.fetch_add(1, Ordering::Relaxed);
            if consume_failure(&self.multipart_abort_failures_remaining) {
                return Err(injected_network_error("multipart abort"));
            }
            let existed = self
                .multipart_uploads
                .lock()
                .expect("multipart uploads lock")
                .remove(&session.upload_id)
                .is_some();
            if !existed {
                // Faithful to the real S3 NoSuchUpload on a repeated abort
                // after a crash between remote success and local removal.
                // Only this definitive absent error is idempotent success.
                return Err(BackendError::NotFound {
                    object: session.destination.clone(),
                });
            }
            if consume_failure(&self.multipart_interrupt_after_abort) {
                self.multipart_interrupted.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(())
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
            object: &ObjectRef,
            _mode: DeleteMode,
        ) -> Result<OperationTicket, BackendError> {
            self.deleted
                .lock()
                .expect("deleted lock")
                .push(object.display_path.clone());
            self.files
                .lock()
                .expect("files lock")
                .remove(&object.display_path);
            Ok(OperationTicket {
                id: TicketId(2),
                kind: OperationKind::Delete,
            })
        }

        async fn get_metadata(
            &self,
            object: &ObjectRef,
            level: MetadataLevel,
        ) -> Result<MetadataResult, BackendError> {
            let len = self
                .files
                .lock()
                .expect("files lock")
                .get(&object.display_path)
                .map(|v| v.len() as u64)
                .unwrap_or(0);
            Ok(MetadataResult {
                level,
                is_dir: false,
                size: Some(len),
                mtime: None,
                ctime: None,
                content_type: None,
                etag: Some(format!("etag-{}", len)),
            })
        }

        async fn get_stable_id(&self, object: &ObjectRef) -> Result<StableId, BackendError> {
            Ok(object.key.stable_id.clone())
        }

        fn capabilities(&self) -> CapabilitySet {
            CapabilitySet {
                can_range_read: true,
                can_range_write: true,
                can_rename: true,
                can_provide_progress: true,
                can_verify_by_checksum: true,
                can_etag: true,
                ..CapabilitySet::default()
            }
        }
    }

    fn consume_failure(remaining: &AtomicU32) -> bool {
        remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_sub(1)
            })
            .is_ok()
    }

    fn injected_network_error(operation: &'static str) -> BackendError {
        BackendError::Network(NetworkErrorDetail {
            message: format!("injected {operation} failure"),
            is_timeout: false,
        })
    }

    #[tokio::test]
    async fn move_via_relay_with_verify_does_not_delete_on_mismatch() {
        let src_backend = MockBackend::new("src", false);
        let dst_backend = MockBackend::new("dst", true);

        let src = make_ref("src", "a.bin");
        let dst = make_ref("dst", "b.bin");
        src_backend.put("a.bin", b"source-bytes");

        let (tx, _rx) = watch::channel(ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: None,
        });

        let result = move_via_relay_with_verify_and_control(
            &src_backend,
            &src,
            &dst_backend,
            &dst,
            &tx,
            MoveRelayOptions::default(),
        )
        .await;

        assert!(result.is_err());
        assert!(src_backend.deleted_paths().is_empty());
    }

    #[tokio::test]
    async fn client_relay_copy_rejects_short_stream_without_full_completion_progress() {
        let src_backend = MockBackend::new("src", false);
        src_backend.put("from.bin", b"abcdef");
        src_backend.short_next_reads(1);
        let dst_root = tempdir().expect("create dst tempdir");
        let dst_backend = LocalFsAdapter::new(
            dst_root.path().to_path_buf(),
            BackendId("local-dst".to_string()),
        );
        let (tx, rx) = watch::channel(ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: None,
        });

        let error = client_relay_copy(
            &src_backend,
            &make_ref("src", "from.bin"),
            &dst_backend,
            &make_ref("local-dst", "to.bin"),
            &tx,
            CopyRelayOptions::default(),
        )
        .await
        .expect_err("short stream must fail the relay");

        let BackendError::Internal(error) = error else {
            panic!("expected an I/O error");
        };
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        assert!(matches!(src_backend.read_ranges().as_slice(), [None]));
        assert_eq!(
            fs::read(dst_root.path().join("to.bin")).await.unwrap(),
            b"abc"
        );
        let progress = rx.borrow();
        assert_eq!(progress.files_completed, 0);
        assert!(progress.bytes_completed < 6);
    }

    #[tokio::test]
    async fn client_relay_copy_prefers_streaming_destination_write() {
        let src_backend = MockBackend::new("src", false);
        let dst_backend = MockBackend::new("dst", false);

        let src = make_ref("src", "from.bin");
        let dst = make_ref("dst", "to.bin");
        src_backend.put("from.bin", b"abcdef");

        let (tx, _rx) = watch::channel(ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: None,
        });

        let result = client_relay_copy(
            &src_backend,
            &src,
            &dst_backend,
            &dst,
            &tx,
            CopyRelayOptions::default(),
        )
        .await
        .expect("relay copy should succeed");

        assert_eq!(result.bytes_transferred, 6);
        assert_eq!(dst_backend.stream_write_count(), 1);
    }

    #[tokio::test]
    async fn client_relay_copy_uses_chunked_range_reads_for_large_sources() {
        let src_backend = MockBackend::new("s3", false);
        let dst_backend = MockBackend::new("local", false);

        let total_len = crate::chunked::CHUNKED_RANGE_MIN_BYTES + 17;
        let mut payload = vec![0u8; total_len as usize];
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        src_backend.put("large.bin", &payload);

        let src = make_ref("s3", "large.bin");
        let dst = make_ref("local", "large-copy.bin");
        let (tx, _rx) = watch::channel(ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: None,
        });

        let result = client_relay_copy(
            &src_backend,
            &src,
            &dst_backend,
            &dst,
            &tx,
            CopyRelayOptions::default(),
        )
        .await
        .expect("chunked relay copy should succeed");

        assert_eq!(result.bytes_transferred, total_len);
        assert_eq!(dst_backend.get("large-copy.bin"), Some(payload));

        let ranges = src_backend.read_ranges();
        assert!(ranges.iter().all(Option::is_some));
        assert_eq!(
            ranges
                .iter()
                .filter_map(|range| range.as_ref())
                .map(|range| range.offset)
                .collect::<Vec<_>>(),
            vec![
                0,
                crate::chunked::CHUNKED_RANGE_CHUNK_BYTES,
                crate::chunked::CHUNKED_RANGE_CHUNK_BYTES * 2
            ]
        );
    }

    #[tokio::test]
    async fn chunked_copy_retries_transient_range_read_and_write_failures() {
        let src_backend = MockBackend::new("s3:retry-source", false);
        let dst_backend = MockBackend::new("s3:retry-destination", false);
        let total_len = crate::chunked::CHUNKED_RANGE_MIN_BYTES + 17;
        let payload = (0..total_len)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        src_backend.put("retry.bin", &payload);
        src_backend.fail_next_reads(2);
        src_backend.short_next_reads(1);
        dst_backend.fail_next_range_writes(1);

        let src = make_ref("s3:retry-source", "retry.bin");
        let dst = make_ref("s3:retry-destination", "retry-copy.bin");
        let (tx, _rx) = watch::channel(ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: None,
        });
        let retry_policy = RetryPolicy {
            max_retries: 3,
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            backoff_factor: 2.0,
        };

        let result = client_relay_copy(
            &src_backend,
            &src,
            &dst_backend,
            &dst,
            &tx,
            CopyRelayOptions {
                control: None,
                resumable: None,
                retry_policy: Some(&retry_policy),
            },
        )
        .await
        .expect("transient failures should be retried");

        assert_eq!(result.bytes_transferred, total_len);
        assert_eq!(dst_backend.get("retry-copy.bin"), Some(payload));
        assert_eq!(src_backend.read_ranges().len(), 6);
        assert_eq!(dst_backend.range_write_count(), 4);
    }

    #[tokio::test]
    async fn client_relay_copy_records_resumable_chunk_states() {
        let src_backend = MockBackend::new("s3", false);
        let dst_backend = MockBackend::new("local", false);
        let store_dir = tempdir().expect("jobstore tempdir");
        let job_store = Arc::new(
            JobStore::open(&store_dir.path().join("jobs.sqlite3")).expect("open jobstore"),
        );
        let job_id = JobId(41);
        job_store
            .save_job(&PersistedJob {
                id: job_id.clone(),
                state: JobState::Running,
                created_at: "2026-07-09T00:00:00Z".into(),
                updated_at: "2026-07-09T00:00:00Z".into(),
                description: "resumable copy".into(),
                tasks: vec![PersistedTask {
                    id: TaskId(7),
                    job_id: job_id.clone(),
                    kind: TaskKind::Copy,
                    state: TaskState::Running,
                    src_backend: Some("s3".into()),
                    src_stable_id: Some("test".into()),
                    src_display_path: Some("resume.bin".into()),
                    dst_backend: Some("local".into()),
                    dst_stable_id: Some("test".into()),
                    dst_display_path: Some("resume-copy.bin".into()),
                    bytes_total: 0,
                    bytes_completed: 0,
                    error_message: None,
                }],
            })
            .expect("save job");

        let total_len = crate::chunked::CHUNKED_RANGE_MIN_BYTES + 1;
        let payload = vec![42u8; total_len as usize];
        src_backend.put("resume.bin", &payload);

        let src = make_ref("s3", "resume.bin");
        let dst = make_ref("local", "resume-copy.bin");
        let resumable = ResumableTransferContext {
            job_store: Arc::clone(&job_store),
            job_id: job_id.clone(),
            entry_index: 7,
        };
        let (tx, _rx) = watch::channel(ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: None,
        });

        client_relay_copy(
            &src_backend,
            &src,
            &dst_backend,
            &dst,
            &tx,
            CopyRelayOptions {
                control: None,
                resumable: Some(&resumable),
                retry_policy: None,
            },
        )
        .await
        .expect("resumable chunked copy should succeed");

        let chunks = job_store
            .load_chunk_states(&job_id)
            .expect("load chunk states");
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|chunk| chunk.entry_index == 7));
        assert!(chunks
            .iter()
            .all(|chunk| chunk.state == PersistedChunkState::Completed));
        assert_eq!(
            chunks.iter().map(|chunk| chunk.offset).collect::<Vec<_>>(),
            vec![
                0,
                crate::chunked::CHUNKED_RANGE_CHUNK_BYTES,
                crate::chunked::CHUNKED_RANGE_CHUNK_BYTES * 2
            ]
        );
        assert!(chunks
            .iter()
            .all(|chunk| chunk.etag == Some(format!("etag-{total_len}"))));
    }

    #[tokio::test]
    async fn multipart_completion_recovery_never_confuses_conflict_cleanup_with_success() {
        // Exercise normal cleanup, retryable abort failure, and a crash after
        // remote abort but before local removal. Also retain lost-response recovery
        // for an upload whose completion was not known to have conflicted.
        for scenario in [
            "abort",
            "abort failure",
            "interrupted abort",
            "lost completion",
        ] {
            let src_backend = MockBackend::new("s3:source", false);
            let mut dst_backend = MockBackend::new("s3-multipart:destination", false);
            let total_len = crate::chunked::CHUNKED_RANGE_MIN_BYTES;
            src_backend.put("source.bin", &vec![1; total_len as usize]);
            if scenario == "lost completion" {
                dst_backend.multipart_interrupt_after_completion = true;
            } else {
                dst_backend.multipart_competing_data = Some(vec![2; total_len as usize]);
            }
            if scenario == "abort failure" {
                dst_backend.fail_next_aborts(1);
            } else if scenario == "interrupted abort" {
                dst_backend
                    .multipart_interrupt_after_abort
                    .store(1, Ordering::Relaxed);
            }
            let src = make_ref("s3:source", "source.bin");
            let dst = make_ref("s3-multipart:destination", "destination.bin");
            let dir = tempdir().unwrap();
            let path = dir.path().join("jobs.sqlite3");
            let store = Arc::new(JobStore::open(&path).unwrap());
            let job_id = JobId(44);
            store
                .save_job(&PersistedJob {
                    id: job_id.clone(),
                    state: JobState::Running,
                    created_at: "2026-07-10T00:00:00Z".into(),
                    updated_at: "2026-07-10T00:00:00Z".into(),
                    description: "conditional completion recovery".into(),
                    tasks: Vec::new(),
                })
                .unwrap();
            let (tx, _rx) = watch::channel(ProgressInfo::single_file(0, Some(total_len), false));
            {
                let context = ResumableTransferContext {
                    job_store: Arc::clone(&store),
                    job_id: job_id.clone(),
                    entry_index: 0,
                };
                let transfer = move_via_relay_with_verify_and_control(
                    &src_backend,
                    &src,
                    &dst_backend,
                    &dst,
                    &tx,
                    MoveRelayOptions {
                        resumable: Some(&context),
                        ..MoveRelayOptions::default()
                    },
                );
                tokio::pin!(transfer);
                if matches!(scenario, "interrupted abort" | "lost completion") {
                    tokio::select! {
                        result = &mut transfer => panic!("expected interruption, got {result:?}"),
                        () = dst_backend.multipart_interrupted.notified() => {},
                    }
                    // Dropping the future models exit after the remote side effect,
                    // before the local record can be removed.
                } else {
                    assert!(matches!(transfer.await, Err(BackendError::Conflict { .. })));
                }
            }
            assert!(src_backend.deleted_paths().is_empty());
            assert!(
                dst_backend.multipart_uploads.lock().unwrap().is_empty()
                    || scenario == "abort failure"
            );
            drop(store);
            let store = Arc::new(JobStore::open(&path).unwrap());
            let record = store.load_multipart_upload(&job_id, 0).unwrap();
            if scenario == "abort" {
                assert!(record.is_none());
                continue;
            }
            assert_eq!(
                record.unwrap().cleanup_pending,
                scenario != "lost completion"
            );
            let context = ResumableTransferContext {
                job_store: Arc::clone(&store),
                job_id: job_id.clone(),
                entry_index: 0,
            };
            let result = move_via_relay_with_verify_and_control(
                &src_backend,
                &src,
                &dst_backend,
                &dst,
                &tx,
                MoveRelayOptions {
                    resumable: Some(&context),
                    ..MoveRelayOptions::default()
                },
            )
            .await;
            if scenario == "lost completion" {
                result.expect("unrejected completion still recovers after a lost response");
                assert_eq!(src_backend.deleted_paths(), vec!["source.bin"]);
                assert_eq!(
                    dst_backend.get("destination.bin"),
                    Some(vec![1; total_len as usize])
                );
            } else {
                assert!(matches!(result, Err(BackendError::Conflict { .. })));
                assert!(
                    src_backend.deleted_paths().is_empty(),
                    "conflict must not reach source deletion"
                );
                assert_eq!(
                    src_backend.get("source.bin"),
                    Some(vec![1; total_len as usize])
                );
                assert_eq!(
                    dst_backend.get("destination.bin"),
                    Some(vec![2; total_len as usize])
                );
                assert_eq!(dst_backend.abort_call_count(), 2);
            }
            assert_eq!(dst_backend.created_multipart_uploads(), 1);
            assert!(store.load_multipart_upload(&job_id, 0).unwrap().is_none());
            assert!(dst_backend.multipart_uploads.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn multipart_list_error_verifies_content_before_recovery() {
        use iris_jobstore::repository::PersistedMultipartUpload;
        async fn setup_list_store(
            dst_bytes: &[u8],
            fail_dst_reads: u32,
        ) -> (
            tempfile::TempDir,
            MockBackend,
            MockBackend,
            Arc<JobStore>,
            JobId,
        ) {
            let src_backend = MockBackend::new("s3:source", false);
            let dst_backend = MockBackend::new("s3-multipart:destination", false);
            src_backend.put("source.bin", b"abc");
            dst_backend.put("destination.bin", dst_bytes);
            if fail_dst_reads > 0 {
                dst_backend.fail_next_reads(fail_dst_reads);
            }
            let dir = tempdir().unwrap();
            let path = dir.path().join("jobs.sqlite3");
            let store = Arc::new(JobStore::open(&path).unwrap());
            let job_id = JobId(91);
            store
                .save_job(&PersistedJob {
                    id: job_id.clone(),
                    state: JobState::Running,
                    created_at: "2026-07-10T00:00:00Z".into(),
                    updated_at: "2026-07-10T00:00:00Z".into(),
                    description: "list recovery".into(),
                    tasks: Vec::new(),
                })
                .unwrap();
            // A valid record whose remote upload is gone forces the
            // ListParts-error recovery path on the next resume.
            store
                .save_multipart_upload(&PersistedMultipartUpload {
                    job_id: job_id.clone(),
                    entry_index: 0,
                    destination_backend: "s3-multipart:destination".into(),
                    destination_path: "destination.bin".into(),
                    source_identity: Some("source-etag".into()),
                    upload_id: "missing-upload".into(),
                    total_len: 3,
                    part_size: 2,
                    cleanup_pending: false,
                })
                .unwrap();
            (dir, src_backend, dst_backend, store, job_id)
        }
        async fn run_list_copy(
            src_backend: &MockBackend,
            dst_backend: &MockBackend,
            store: &Arc<JobStore>,
            job_id: &JobId,
        ) -> Result<(), BackendError> {
            let (tx, _rx) = watch::channel(ProgressInfo::single_file(0, Some(3), false));
            let (_pause_tx, pause_rx) = watch::channel(false);
            copy_chunked_to_multipart(MultipartCopyRequest {
                src_backend,
                src: make_ref("s3:source", "source.bin"),
                dst_backend,
                dst: &make_ref("s3-multipart:destination", "destination.bin"),
                total_len: 3,
                part_size: 2,
                source_identity: Some("source-etag".into()),
                requested_in_flight: 1,
                job_store: Arc::clone(store),
                job_id: job_id.clone(),
                entry_index: 0,
                progress_tx: &tx,
                retry: ChunkRetryControls {
                    policy: RetryPolicy {
                        max_retries: 0,
                        initial_delay: Duration::ZERO,
                        max_delay: Duration::ZERO,
                        backoff_factor: 2.0,
                    },
                    pause_rx,
                    cancel_token: CancellationToken::new(),
                },
            })
            .await
        }
        // A missing upload plus same-size competing bytes must never recover
        // as success.
        {
            let (_dir, src, dst, store, job_id) = setup_list_store(b"xyz", 0).await;
            let result = run_list_copy(&src, &dst, &store, &job_id).await;
            assert!(
                matches!(result, Err(BackendError::Conflict { .. })),
                "same-size competitor must conflict, got {result:?}"
            );
            assert_eq!(dst.get("destination.bin"), Some(b"xyz".to_vec()));
        }
        // A transient ListParts failure is not evidence the upload disappeared;
        // keep the persisted session instead of creating an orphan replacement.
        {
            let (_dir, src, dst, store, job_id) = setup_list_store(b"xyz", 0).await;
            let session = dst
                .create_multipart_upload(
                    &make_ref("s3-multipart:destination", "destination.bin"),
                    ConflictResolution::Fail,
                )
                .await
                .unwrap();
            store
                .save_multipart_upload(&PersistedMultipartUpload {
                    job_id: job_id.clone(),
                    entry_index: 0,
                    destination_backend: "s3-multipart:destination".into(),
                    destination_path: "destination.bin".into(),
                    source_identity: Some("source-etag".into()),
                    upload_id: session.upload_id.clone(),
                    total_len: 3,
                    part_size: 2,
                    cleanup_pending: false,
                })
                .unwrap();
            dst.fail_next_lists(1);
            let result = run_list_copy(&src, &dst, &store, &job_id).await;
            assert!(matches!(result, Err(BackendError::Network(_))));
            assert_eq!(dst.created_multipart_uploads(), 1);
            assert_eq!(
                store
                    .load_multipart_upload(&job_id, 0)
                    .unwrap()
                    .unwrap()
                    .upload_id,
                session.upload_id
            );
            assert!(dst.list_multipart_parts(&session).await.is_ok());
        }
        // Byte-identical destination recovers a lost response.
        {
            let (_dir, src, dst, store, job_id) = setup_list_store(b"abc", 0).await;
            run_list_copy(&src, &dst, &store, &job_id)
                .await
                .expect("exact content must recover");
            assert!(store.load_multipart_upload(&job_id, 0).unwrap().is_none());
            assert_eq!(dst.get("destination.bin"), Some(b"abc".to_vec()));
        }
        // An unverifiable destination must not report success.
        {
            let (_dir, src, dst, store, job_id) = setup_list_store(b"abc", 1).await;
            let result = run_list_copy(&src, &dst, &store, &job_id).await;
            assert!(result.is_err(), "unverifiable destination must not succeed");
            assert!(store.load_multipart_upload(&job_id, 0).unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn multipart_move_never_deletes_source_without_byte_identical_recovery() {
        // A same-size competitor behind a completion error must keep the source.
        {
            let src_backend = MockBackend::new("s3:source", false);
            let dst_backend = MockBackend::new("s3-multipart:destination", false);
            let total_len = crate::chunked::CHUNKED_RANGE_MIN_BYTES;
            src_backend.put("source.bin", &vec![1; total_len as usize]);
            dst_backend.put("destination.bin", &vec![2; total_len as usize]);
            dst_backend.fail_next_completes(1);
            let src = make_ref("s3:source", "source.bin");
            let dst = make_ref("s3-multipart:destination", "destination.bin");
            let dir = tempdir().unwrap();
            let path = dir.path().join("jobs.sqlite3");
            let store = Arc::new(JobStore::open(&path).unwrap());
            let job_id = JobId(93);
            store
                .save_job(&PersistedJob {
                    id: job_id.clone(),
                    state: JobState::Running,
                    created_at: "2026-07-10T00:00:00Z".into(),
                    updated_at: "2026-07-10T00:00:00Z".into(),
                    description: "move competitor".into(),
                    tasks: Vec::new(),
                })
                .unwrap();
            let (tx, _rx) = watch::channel(ProgressInfo::single_file(0, Some(total_len), false));
            let context = ResumableTransferContext {
                job_store: Arc::clone(&store),
                job_id: job_id.clone(),
                entry_index: 0,
            };
            let no_retries = RetryPolicy {
                max_retries: 0,
                initial_delay: Duration::ZERO,
                max_delay: Duration::ZERO,
                backoff_factor: 2.0,
            };
            let result = move_via_relay_with_verify_and_control(
                &src_backend,
                &src,
                &dst_backend,
                &dst,
                &tx,
                MoveRelayOptions {
                    resumable: Some(&context),
                    retry_policy: Some(&no_retries),
                    ..MoveRelayOptions::default()
                },
            )
            .await;
            assert!(result.is_err(), "competitor must not move, got {result:?}");
            assert!(
                src_backend.deleted_paths().is_empty(),
                "competitor must not delete the source"
            );
            assert_eq!(
                dst_backend.get("destination.bin"),
                Some(vec![2; total_len as usize])
            );
            assert!(store.load_multipart_upload(&job_id, 0).unwrap().is_some());
            assert_eq!(dst_backend.multipart_uploads.lock().unwrap().len(), 1);
        }
        // Byte-identical destination behind a completion error is safe only
        // after aborting the still-live upload.

        {
            let src_backend = MockBackend::new("s3:source", false);
            let dst_backend = MockBackend::new("s3-multipart:destination", false);
            let total_len = crate::chunked::CHUNKED_RANGE_MIN_BYTES;
            src_backend.put("source.bin", &vec![1; total_len as usize]);
            dst_backend.put("destination.bin", &vec![1; total_len as usize]);
            dst_backend.fail_next_completes(1);
            let src = make_ref("s3:source", "source.bin");
            let dst = make_ref("s3-multipart:destination", "destination.bin");
            let dir = tempdir().unwrap();
            let path = dir.path().join("jobs.sqlite3");
            let store = Arc::new(JobStore::open(&path).unwrap());
            let job_id = JobId(94);
            store
                .save_job(&PersistedJob {
                    id: job_id.clone(),
                    state: JobState::Running,
                    created_at: "2026-07-10T00:00:00Z".into(),
                    updated_at: "2026-07-10T00:00:00Z".into(),
                    description: "move exact".into(),
                    tasks: Vec::new(),
                })
                .unwrap();
            let (tx, _rx) = watch::channel(ProgressInfo::single_file(0, Some(total_len), false));
            let context = ResumableTransferContext {
                job_store: Arc::clone(&store),
                job_id: job_id.clone(),
                entry_index: 0,
            };
            let no_retries = RetryPolicy {
                max_retries: 0,
                initial_delay: Duration::ZERO,
                max_delay: Duration::ZERO,
                backoff_factor: 2.0,
            };
            move_via_relay_with_verify_and_control(
                &src_backend,
                &src,
                &dst_backend,
                &dst,
                &tx,
                MoveRelayOptions {
                    resumable: Some(&context),
                    retry_policy: Some(&no_retries),
                    ..MoveRelayOptions::default()
                },
            )
            .await
            .expect("exact content must recover and move");
            assert_eq!(src_backend.deleted_paths(), vec!["source.bin"]);
            assert_eq!(dst_backend.abort_call_count(), 1);
            assert!(dst_backend.multipart_uploads.lock().unwrap().is_empty());
            assert!(store.load_multipart_upload(&job_id, 0).unwrap().is_none());
        }
        // If cleanup of a live upload fails, neither the source nor journal
        // can be discarded even though matching bytes already exist.
        {
            let src_backend = MockBackend::new("s3:source", false);
            let dst_backend = MockBackend::new("s3-multipart:destination", false);
            let total_len = crate::chunked::CHUNKED_RANGE_MIN_BYTES;
            src_backend.put("source.bin", &vec![1; total_len as usize]);
            dst_backend.put("destination.bin", &vec![1; total_len as usize]);
            dst_backend.fail_next_completes(1);
            dst_backend.fail_next_aborts(1);
            let src = make_ref("s3:source", "source.bin");
            let dst = make_ref("s3-multipart:destination", "destination.bin");
            let dir = tempdir().unwrap();
            let path = dir.path().join("jobs.sqlite3");
            let store = Arc::new(JobStore::open(&path).unwrap());
            let job_id = JobId(95);
            store
                .save_job(&PersistedJob {
                    id: job_id.clone(),
                    state: JobState::Running,
                    created_at: "2026-07-10T00:00:00Z".into(),
                    updated_at: "2026-07-10T00:00:00Z".into(),
                    description: "move cleanup failure".into(),
                    tasks: Vec::new(),
                })
                .unwrap();
            let (tx, _rx) = watch::channel(ProgressInfo::single_file(0, Some(total_len), false));
            let context = ResumableTransferContext {
                job_store: Arc::clone(&store),
                job_id: job_id.clone(),
                entry_index: 0,
            };
            let no_retries = RetryPolicy {
                max_retries: 0,
                initial_delay: Duration::ZERO,
                max_delay: Duration::ZERO,
                backoff_factor: 2.0,
            };
            let result = move_via_relay_with_verify_and_control(
                &src_backend,
                &src,
                &dst_backend,
                &dst,
                &tx,
                MoveRelayOptions {
                    resumable: Some(&context),
                    retry_policy: Some(&no_retries),
                    ..MoveRelayOptions::default()
                },
            )
            .await;
            assert!(matches!(result, Err(BackendError::Network(_))));
            assert!(src_backend.deleted_paths().is_empty());
            assert!(store.load_multipart_upload(&job_id, 0).unwrap().is_some());
            assert_eq!(dst_backend.multipart_uploads.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn multipart_copy_resumes_remote_parts_after_jobstore_reopen() {
        let src_backend = MockBackend::new("s3:multipart-source", false);
        let dst_backend = MockBackend::new("s3-multipart:destination", false);
        let store_dir = tempdir().expect("jobstore tempdir");
        let db_path = store_dir.path().join("jobs.sqlite3");
        let job_id = JobId(43);
        let total_len = crate::chunked::CHUNKED_RANGE_MIN_BYTES + 1;
        let payload = (0..total_len)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        src_backend.put("resume-upload.bin", &payload);
        let src = make_ref("s3:multipart-source", "resume-upload.bin");
        let dst = make_ref("s3-multipart:destination", "uploaded.bin");
        let (tx, _rx) = watch::channel(ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: None,
        });

        let first_store = Arc::new(JobStore::open(&db_path).expect("open first jobstore"));
        first_store
            .save_job(&PersistedJob {
                id: job_id.clone(),
                state: JobState::Running,
                created_at: "2026-07-10T00:00:00Z".into(),
                updated_at: "2026-07-10T00:00:00Z".into(),
                description: "resumable multipart copy".into(),
                tasks: Vec::new(),
            })
            .expect("save multipart job");
        let first_context = ResumableTransferContext {
            job_store: Arc::clone(&first_store),
            job_id: job_id.clone(),
            entry_index: 9,
        };
        dst_backend.fail_multipart_after_successes(1);
        let first_result = client_relay_copy(
            &src_backend,
            &src,
            &dst_backend,
            &dst,
            &tx,
            CopyRelayOptions {
                control: None,
                resumable: Some(&first_context),
                retry_policy: None,
            },
        )
        .await;
        assert!(first_result.is_err());
        assert!(first_store
            .load_multipart_upload(&job_id, 9)
            .expect("load interrupted upload")
            .is_some());
        drop(first_context);
        drop(first_store);

        dst_backend.fail_multipart_after_successes(u32::MAX);
        let resumed_store = Arc::new(JobStore::open(&db_path).expect("reopen jobstore"));
        let resumed_context = ResumableTransferContext {
            job_store: Arc::clone(&resumed_store),
            job_id: job_id.clone(),
            entry_index: 9,
        };
        let resumed = client_relay_copy(
            &src_backend,
            &src,
            &dst_backend,
            &dst,
            &tx,
            CopyRelayOptions {
                control: None,
                resumable: Some(&resumed_context),
                retry_policy: None,
            },
        )
        .await
        .expect("multipart upload should resume");

        assert_eq!(resumed.bytes_transferred, total_len);
        assert_eq!(dst_backend.get("uploaded.bin"), Some(payload));
        assert_eq!(dst_backend.created_multipart_uploads(), 1);
        assert!(resumed_store
            .load_multipart_upload(&job_id, 9)
            .expect("load completed upload")
            .is_none());
    }

    #[tokio::test]
    async fn client_relay_copy_skips_completed_resumable_chunks_with_matching_etag() {
        let src_backend = MockBackend::new("s3", false);
        let dst_backend = MockBackend::new("local", false);
        let store_dir = tempdir().expect("jobstore tempdir");
        let job_store = Arc::new(
            JobStore::open(&store_dir.path().join("jobs.sqlite3")).expect("open jobstore"),
        );
        let job_id = JobId(42);
        job_store
            .save_job(&PersistedJob {
                id: job_id.clone(),
                state: JobState::Running,
                created_at: "2026-07-09T00:00:00Z".into(),
                updated_at: "2026-07-09T00:00:00Z".into(),
                description: "resumable copy with completed chunk".into(),
                tasks: vec![PersistedTask {
                    id: TaskId(7),
                    job_id: job_id.clone(),
                    kind: TaskKind::Copy,
                    state: TaskState::Running,
                    src_backend: Some("s3".into()),
                    src_stable_id: Some("test".into()),
                    src_display_path: Some("resume.bin".into()),
                    dst_backend: Some("local".into()),
                    dst_stable_id: Some("test".into()),
                    dst_display_path: Some("resume-copy.bin".into()),
                    bytes_total: 0,
                    bytes_completed: 0,
                    error_message: None,
                }],
            })
            .expect("save job");

        let total_len = crate::chunked::CHUNKED_RANGE_MIN_BYTES + 1;
        let mut payload = vec![0u8; total_len as usize];
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        src_backend.put("resume.bin", &payload);

        let first_chunk_len = crate::chunked::CHUNKED_RANGE_CHUNK_BYTES as usize;
        let mut partial = vec![0u8; total_len as usize];
        partial[..first_chunk_len].copy_from_slice(&payload[..first_chunk_len]);
        dst_backend.put("resume-copy.bin", &partial);
        job_store
            .upsert_chunk_state(&PersistedChunkStateRow {
                job_id: job_id.clone(),
                entry_index: 7,
                offset: 0,
                len: crate::chunked::CHUNKED_RANGE_CHUNK_BYTES,
                state: PersistedChunkState::Completed,
                etag: Some(format!("etag-{total_len}")),
                checksum: None,
            })
            .expect("save completed chunk");
        job_store.flush().expect("flush completed chunk");

        let src = make_ref("s3", "resume.bin");
        let dst = make_ref("local", "resume-copy.bin");
        let resumable = ResumableTransferContext {
            job_store: Arc::clone(&job_store),
            job_id: job_id.clone(),
            entry_index: 7,
        };
        let (tx, _rx) = watch::channel(ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: None,
        });

        client_relay_copy(
            &src_backend,
            &src,
            &dst_backend,
            &dst,
            &tx,
            CopyRelayOptions {
                control: None,
                resumable: Some(&resumable),
                retry_policy: None,
            },
        )
        .await
        .expect("resumable chunked copy should succeed");

        assert_eq!(dst_backend.get("resume-copy.bin"), Some(payload));
        let mut read_offsets = src_backend
            .read_ranges()
            .into_iter()
            .map(|range| range.expect("range read").offset)
            .collect::<Vec<_>>();
        read_offsets.sort_unstable();
        assert_eq!(
            read_offsets,
            vec![
                0,
                crate::chunked::CHUNKED_RANGE_CHUNK_BYTES,
                crate::chunked::CHUNKED_RANGE_CHUNK_BYTES * 2
            ]
        );

        let chunks = job_store
            .load_chunk_states(&job_id)
            .expect("load chunk states");
        assert_eq!(chunks.len(), 3);
        assert!(chunks
            .iter()
            .all(|chunk| chunk.state == PersistedChunkState::Completed));
    }
}
