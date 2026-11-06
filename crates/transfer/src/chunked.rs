use crate::error_handler::RetryPolicy;
use crate::method_selector::global_concurrency_governor;
use crate::verify::{verify_strong, VerifyResult};
use futures_util::stream::{FuturesUnordered, Stream, StreamExt};
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::memory_budget::{MemoryBudget, MemoryBudgetPermit};
use iris_core_domain::types::error::{BackendError, NetworkErrorDetail};
use iris_core_domain::types::multipart::{MultipartPart, MultipartUploadSession};
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_core_domain::types::options::ConflictResolution;
use iris_core_domain::types::progress::{ByteRange, ProgressInfo};
use iris_core_domain::types::transfer::JobId;
use iris_jobstore::repository::{
    JobStoreError, PersistedChunkState, PersistedChunkStateRow, PersistedMultipartUpload,
};
use iris_jobstore::writer_task::JobStore;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::io::Cursor;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio::sync::watch;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

pub(crate) const CHUNKED_RANGE_CHUNK_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const CHUNKED_RANGE_MIN_BYTES: u64 = CHUNKED_RANGE_CHUNK_BYTES * 2;
pub(crate) const CHUNKED_RANGE_MAX_BUFFERED_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const CHUNKED_RANGE_DEFAULT_IN_FLIGHT: usize = 8;
const CHUNKED_RANGE_MAX_IN_FLIGHT: usize = 16;
const TRANSFER_MEMORY_BUDGET_EXCEEDED_OPERATION: &str =
    "transfer range buffer exceeds memory budget";

type ChunkFuture<'a> = Pin<Box<dyn Future<Output = Result<RangeChunk, BackendError>> + Send + 'a>>;

#[derive(Clone)]
pub(crate) struct ChunkRetryControls {
    pub(crate) policy: RetryPolicy,
    pub(crate) pause_rx: watch::Receiver<bool>,
    pub(crate) cancel_token: CancellationToken,
}

#[derive(Clone)]
pub(crate) struct ChunkStateRecorder {
    job_store: Arc<JobStore>,
    job_id: JobId,
    entry_index: u64,
    etag: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ChunkedTransferPlan {
    max_in_flight: usize,
    ranges: Vec<ByteRange>,
}

#[derive(Debug)]
struct RangeChunk {
    index: usize,
    range: ByteRange,
    bytes: Vec<u8>,
    _permit: MemoryBudgetPermit,
}

#[derive(Debug)]
struct BufferedRangeChunk {
    bytes: Vec<u8>,
    _permit: MemoryBudgetPermit,
}

pub(crate) struct ChunkedRangeReader<'a> {
    backend: &'a dyn BackendAdapter,
    object: ObjectRef,
    plan: ChunkedTransferPlan,
    recorder: Option<ChunkStateRecorder>,
    next_to_schedule: usize,
    next_to_emit: usize,
    in_flight: FuturesUnordered<ChunkFuture<'a>>,
    ready: BTreeMap<usize, BufferedRangeChunk>,
    current: Option<(BufferedRangeChunk, usize)>,
    retry: ChunkRetryControls,
}

impl<'a> ChunkedRangeReader<'a> {
    fn with_plan(
        backend: &'a dyn BackendAdapter,
        object: ObjectRef,
        plan: ChunkedTransferPlan,
        recorder: Option<ChunkStateRecorder>,
        retry: ChunkRetryControls,
    ) -> Self {
        Self {
            backend,
            object,
            plan,
            recorder,
            next_to_schedule: 0,
            next_to_emit: 0,
            in_flight: FuturesUnordered::new(),
            ready: BTreeMap::new(),
            current: None,
            retry,
        }
    }

    fn schedule_available(&mut self) {
        while self.in_flight.len() < self.plan.max_in_flight
            && self.next_to_schedule < self.plan.ranges.len()
        {
            let index = self.next_to_schedule;
            let range = self.plan.ranges[index].clone();
            self.in_flight.push(read_range_chunk(
                self.backend,
                self.object.clone(),
                index,
                range,
                self.recorder.clone(),
                self.retry.clone(),
            ));
            self.next_to_schedule += 1;
        }
    }

    fn load_ready_chunk(&mut self) -> bool {
        let Some(chunk) = self.ready.remove(&self.next_to_emit) else {
            return false;
        };
        self.next_to_emit += 1;
        if chunk.bytes.is_empty() {
            return true;
        }
        self.current = Some((chunk, 0));
        true
    }

    fn copy_current_to(&mut self, buf: &mut ReadBuf<'_>) -> bool {
        let Some((chunk, offset)) = self.current.as_mut() else {
            return false;
        };
        let remaining = &chunk.bytes[*offset..];
        let to_copy = remaining.len().min(buf.remaining());
        buf.put_slice(&remaining[..to_copy]);
        *offset += to_copy;
        if *offset == chunk.bytes.len() {
            self.current = None;
        }
        to_copy > 0
    }

    fn is_complete(&self) -> bool {
        self.next_to_emit == self.plan.ranges.len()
            && self.current.is_none()
            && self.ready.is_empty()
            && self.in_flight.is_empty()
    }
}

impl AsyncRead for ChunkedRangeReader<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }

        loop {
            if self.copy_current_to(buf) {
                return Poll::Ready(Ok(()));
            }

            if self.load_ready_chunk() {
                continue;
            }

            if self.is_complete() {
                return Poll::Ready(Ok(()));
            }

            self.schedule_available();
            match Pin::new(&mut self.in_flight).poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    self.ready.insert(
                        chunk.index,
                        BufferedRangeChunk {
                            bytes: chunk.bytes,
                            _permit: chunk._permit,
                        },
                    );
                }
                Poll::Ready(Some(Err(error))) => {
                    return Poll::Ready(Err(std::io::Error::other(format!(
                        "range read failed: {error}"
                    ))));
                }
                Poll::Ready(None) => {
                    if self.is_complete() {
                        return Poll::Ready(Ok(()));
                    }
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl ChunkStateRecorder {
    pub(crate) fn new(
        job_store: Arc<JobStore>,
        job_id: JobId,
        entry_index: u64,
        etag: Option<String>,
    ) -> Self {
        Self {
            job_store,
            job_id,
            entry_index,
            etag,
        }
    }

    fn record_plan(&self, plan: &ChunkedTransferPlan) -> Result<(), BackendError> {
        for range in &plan.ranges {
            self.record(range, PersistedChunkState::Pending, None)?;
        }
        Ok(())
    }

    fn record_pending_ranges(
        &self,
        plan: &ChunkedTransferPlan,
        completed_offsets: &HashSet<u64>,
    ) -> Result<(), BackendError> {
        for range in &plan.ranges {
            if !completed_offsets.contains(&range.offset) {
                self.record(range, PersistedChunkState::Pending, None)?;
            }
        }
        Ok(())
    }

    fn completed_offsets(&self, plan: &ChunkedTransferPlan) -> Result<HashSet<u64>, BackendError> {
        if self.etag.is_none() {
            return Ok(HashSet::new());
        }

        let rows = self
            .job_store
            .load_chunk_states(&self.job_id)
            .map_err(jobstore_error_to_backend_error)?;
        let planned = plan
            .ranges
            .iter()
            .map(|range| (range.offset, range.length.unwrap_or(0)))
            .collect::<HashSet<_>>();

        Ok(rows
            .into_iter()
            .filter(|row| {
                row.entry_index == self.entry_index
                    && row.state == PersistedChunkState::Completed
                    && row.etag == self.etag
                    && planned.contains(&(row.offset, row.len))
            })
            .map(|row| row.offset)
            .collect())
    }

    pub(crate) fn record_transfer_outcome(
        &self,
        total_len: u64,
        state: PersistedChunkState,
    ) -> Result<(), BackendError> {
        let plan = plan_chunked_transfer(total_len, CHUNKED_RANGE_CHUNK_BYTES, 1);
        for range in &plan.ranges {
            self.record(range, state.clone(), None)?;
        }
        self.job_store
            .flush()
            .map_err(jobstore_error_to_backend_error)
    }

    fn record(
        &self,
        range: &ByteRange,
        state: PersistedChunkState,
        checksum: Option<String>,
    ) -> Result<(), BackendError> {
        let chunk = PersistedChunkStateRow {
            job_id: self.job_id.clone(),
            entry_index: self.entry_index,
            offset: range.offset,
            len: range.length.unwrap_or(0),
            state,
            etag: self.etag.clone(),
            checksum,
        };
        self.job_store
            .upsert_chunk_state(&chunk)
            .map_err(jobstore_error_to_backend_error)
    }
}

pub(crate) fn should_use_chunked_range(total_len: Option<u64>, can_range_read: bool) -> bool {
    can_range_read
        && total_len.is_some_and(|len| {
            (CHUNKED_RANGE_MIN_BYTES..=CHUNKED_RANGE_MAX_BUFFERED_BYTES).contains(&len)
        })
}

pub(crate) fn should_use_chunked_range_write(
    total_len: Option<u64>,
    can_range_read: bool,
    can_range_write: bool,
) -> bool {
    can_range_read && can_range_write && total_len.is_some_and(|len| len >= CHUNKED_RANGE_MIN_BYTES)
}

pub(crate) struct ChunkedRangeWriteRequest<'a> {
    pub(crate) src_backend: &'a dyn BackendAdapter,
    pub(crate) src: ObjectRef,
    pub(crate) dst_backend: &'a dyn BackendAdapter,
    pub(crate) dst: &'a ObjectRef,
    pub(crate) total_len: u64,
    pub(crate) requested_in_flight: usize,
    pub(crate) recorder: Option<ChunkStateRecorder>,
    pub(crate) progress_tx: &'a watch::Sender<ProgressInfo>,
    pub(crate) pause_rx: watch::Receiver<bool>,
    pub(crate) cancel_token: CancellationToken,
    pub(crate) retry_policy: RetryPolicy,
}

pub(crate) struct MultipartCopyRequest<'a> {
    pub(crate) src_backend: &'a dyn BackendAdapter,
    pub(crate) src: ObjectRef,
    pub(crate) dst_backend: &'a dyn BackendAdapter,
    pub(crate) dst: &'a ObjectRef,
    pub(crate) total_len: u64,
    pub(crate) part_size: u64,
    pub(crate) source_identity: Option<String>,
    pub(crate) requested_in_flight: usize,
    pub(crate) job_store: Arc<JobStore>,
    pub(crate) job_id: JobId,
    pub(crate) entry_index: u64,
    pub(crate) progress_tx: &'a watch::Sender<ProgressInfo>,
    pub(crate) retry: ChunkRetryControls,
}

struct MultipartJournal<'a> {
    job_store: &'a JobStore,
    job_id: &'a JobId,
    entry_index: u64,
    total_len: u64,
    part_size: u64,
    source_identity: Option<String>,
}

pub(crate) async fn copy_chunked_to_multipart(
    request: MultipartCopyRequest<'_>,
) -> Result<(), BackendError> {
    let MultipartCopyRequest {
        src_backend,
        src,
        dst_backend,
        dst,
        total_len,
        part_size,
        source_identity,
        requested_in_flight,
        job_store,
        job_id,
        entry_index,
        progress_tx,
        retry,
    } = request;
    let plan = plan_chunked_transfer(total_len, part_size, requested_in_flight);
    let _request_budget = request_budget(plan.max_in_flight).await?;
    if plan.ranges.len() > 10_000 {
        return Err(BackendError::Unsupported {
            operation: "multipart upload exceeds 10,000 parts",
        });
    }
    let journal = MultipartJournal {
        job_store: &job_store,
        job_id: &job_id,
        entry_index,
        total_len,
        part_size,
        source_identity: source_identity.clone(),
    };

    let persisted = job_store
        .load_multipart_upload(&job_id, entry_index)
        .map_err(jobstore_error_to_backend_error)?;
    if let Some(upload) = persisted.as_ref().filter(|upload| upload.cleanup_pending) {
        // A known competing object must never enter lost-response recovery,
        // even when the upload has disappeared and the destination size matches.
        if upload.destination_backend == dst.key.backend_id.0 {
            cleanup_conflicted_multipart(
                dst_backend,
                &journal,
                &multipart_session_from_persisted(upload),
            )
            .await?;
        }
        return Err(BackendError::Conflict {
            object: dst.clone(),
        });
    }
    let persisted_is_valid = persisted.as_ref().is_some_and(|upload| {
        upload.destination_backend == dst.key.backend_id.0
            && source_identity.is_some()
            && upload.source_identity == source_identity
            && upload.total_len == total_len
            && upload.part_size == part_size
    });

    let mut session = if persisted_is_valid {
        multipart_session_from_persisted(persisted.as_ref().expect("validated upload exists"))
    } else {
        let replacement =
            create_and_persist_multipart_session(dst_backend, dst, &journal, &retry).await?;
        if let Some(stale) = persisted.as_ref().map(multipart_session_from_persisted) {
            let _ = dst_backend.abort_multipart_upload(&stale).await;
        }
        replacement
    };

    let remote_parts = match retry_backend_operation(
        &retry.policy,
        retry.pause_rx.clone(),
        retry.cancel_token.clone(),
        || dst_backend.list_multipart_parts(&session),
    )
    .await
    {
        Ok(parts) => parts,
        Err(list_error @ BackendError::NotFound { .. }) if persisted_is_valid => {
            // Size alone cannot prove the destination holds our bytes: a
            // same-size competitor must never be mistaken for success.
            // Only byte-identical content recovers. A proven mismatch falls
            // through to a fresh upload (which will conflict if the
            // competitor remains); an unverifiable destination fails closed
            // with the original error so a move never deletes the source.
            match verified_destination_match(src_backend, &src, dst_backend, dst, total_len).await {
                Ok(true) => {
                    job_store
                        .remove_multipart_upload(&job_id, entry_index)
                        .map_err(jobstore_error_to_backend_error)?;
                    return Ok(());
                }
                Ok(false) => {}
                Err(_) => return Err(list_error),
            }

            let replacement =
                create_and_persist_multipart_session(dst_backend, dst, &journal, &retry).await?;
            let _ = dst_backend.abort_multipart_upload(&session).await;
            session = replacement;
            Vec::new()
        }
        Err(error) => return Err(error),
    };

    let mut completed = HashMap::<i32, MultipartPart>::new();
    for part in remote_parts {
        let Some(index) = part
            .part_number
            .checked_sub(1)
            .and_then(|value| usize::try_from(value).ok())
        else {
            continue;
        };
        let Some(range) = plan.ranges.get(index) else {
            continue;
        };
        if part.size != range.length.unwrap_or(0) {
            continue;
        }
        completed.insert(part.part_number, part);
    }

    let mut completed_bytes = completed.values().map(|part| part.size).sum::<u64>();
    let _ = progress_tx.send(ProgressInfo::single_file(
        completed_bytes,
        Some(total_len),
        false,
    ));

    let mut next_to_schedule = 0usize;
    let mut in_flight = FuturesUnordered::new();
    loop {
        wait_for_resume_or_cancel(retry.pause_rx.clone(), retry.cancel_token.clone()).await?;
        while in_flight.len() < plan.max_in_flight && next_to_schedule < plan.ranges.len() {
            let index = next_to_schedule;
            next_to_schedule += 1;
            let part_number = i32::try_from(index + 1).expect("multipart part limit checked");
            if completed.contains_key(&part_number) {
                continue;
            }
            let range = plan.ranges[index].clone();
            let source = src.clone();
            let session = session.clone();
            let retry = retry.clone();
            in_flight.push(async move {
                let chunk = read_range_chunk(
                    src_backend,
                    source,
                    index,
                    range.clone(),
                    None,
                    retry.clone(),
                )
                .await?;
                let bytes: Arc<[u8]> = chunk.bytes.into();
                let part = retry_backend_operation(
                    &retry.policy,
                    retry.pause_rx,
                    retry.cancel_token,
                    || dst_backend.upload_multipart_part(&session, part_number, Arc::clone(&bytes)),
                )
                .await?;
                if part.size != range.length.unwrap_or(0) {
                    return Err(BackendError::Internal(Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "multipart upload reported an unexpected part size",
                    ))));
                }
                Ok::<_, BackendError>(part)
            });
        }

        let Some(result) = in_flight.next().await else {
            break;
        };
        let part = result?;
        completed_bytes = completed_bytes.saturating_add(part.size);
        completed.insert(part.part_number, part);
        let _ = progress_tx.send(ProgressInfo::single_file(
            completed_bytes,
            Some(total_len),
            false,
        ));
    }

    let mut parts = completed.into_values().collect::<Vec<_>>();
    parts.sort_by_key(|part| part.part_number);
    if parts.len() != plan.ranges.len() || completed_bytes != total_len {
        return Err(BackendError::Internal(Box::new(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "multipart upload did not complete every planned part",
        ))));
    }
    let completion =
        retry_backend_operation(&retry.policy, retry.pause_rx, retry.cancel_token, || {
            dst_backend.complete_multipart_upload(&session, parts.clone(), ConflictResolution::Fail)
        })
        .await;
    if let Err(error) = completion {
        // A conditional rejection identifies a competing object, not a lost
        // success response. Equal size cannot establish that it is our data.
        if matches!(error, BackendError::Conflict { .. }) {
            // Commit the rejection before touching the remote session. A crash
            // after abort must leave a cleanup-only record, not a resumable one.
            persist_multipart_session(&journal, &session, true)?;
            cleanup_conflicted_multipart(dst_backend, &journal, &session).await?;
            return Err(error);
        }
        // A non-conflict completion error (for example a lost response) may
        // recover only when the destination holds byte-identical content and
        // this upload is gone or is durably aborted. A live upload plus a
        // matching pre-existing destination is not evidence our completion
        // succeeded, and must not be forgotten from the journal.
        if !destination_holds_source_bytes(src_backend, &src, dst_backend, dst, total_len).await {
            return Err(error);
        }
        match dst_backend.list_multipart_parts(&session).await {
            Err(BackendError::NotFound { .. }) => {}
            Ok(_) => match dst_backend.abort_multipart_upload(&session).await {
                Ok(()) | Err(BackendError::NotFound { .. }) => {}
                Err(_) => return Err(error),
            },
            Err(_) => return Err(error),
        }
    }
    job_store
        .remove_multipart_upload(&job_id, entry_index)
        .map_err(jobstore_error_to_backend_error)?;
    Ok(())
}

async fn create_and_persist_multipart_session(
    backend: &dyn BackendAdapter,
    dst: &ObjectRef,
    journal: &MultipartJournal<'_>,
    retry: &ChunkRetryControls,
) -> Result<MultipartUploadSession, BackendError> {
    let session = retry_backend_operation(
        &retry.policy,
        retry.pause_rx.clone(),
        retry.cancel_token.clone(),
        || backend.create_multipart_upload(dst, ConflictResolution::Fail),
    )
    .await?;
    if let Err(error) = persist_multipart_session(journal, &session, false) {
        let _ = backend.abort_multipart_upload(&session).await;
        return Err(error);
    }
    Ok(session)
}

async fn cleanup_conflicted_multipart(
    backend: &dyn BackendAdapter,
    journal: &MultipartJournal<'_>,
    session: &MultipartUploadSession,
) -> Result<(), BackendError> {
    // Only the definitive already-absent upload error counts as idempotent
    // cleanup success alongside Ok. Uncertain, permission, or service
    // failures keep the record for another resume (or the job's deletion).
    match backend.abort_multipart_upload(session).await {
        Ok(()) | Err(BackendError::NotFound { .. }) => {
            journal
                .job_store
                .remove_multipart_upload(journal.job_id, journal.entry_index)
                .map_err(jobstore_error_to_backend_error)?;
        }
        Err(_) => {}
    }
    Ok(())
}

async fn verified_destination_match(
    src_backend: &dyn BackendAdapter,
    src: &ObjectRef,
    dst_backend: &dyn BackendAdapter,
    dst: &ObjectRef,
    total_len: u64,
) -> Result<bool, BackendError> {
    // A size mismatch already proves the destination is not our data.
    let size_matches = dst_backend
        .stat(dst)
        .await
        .ok()
        .and_then(|metadata| (!metadata.is_dir).then_some(metadata.size).flatten())
        == Some(total_len);
    if !size_matches {
        return Ok(false);
    }
    // Only byte-identical content may recover. A mismatch is a proven
    // competitor; any verification failure leaves the destination
    // unverifiable and must fail closed.
    match verify_strong(src_backend, src, dst_backend, dst).await {
        Ok(VerifyResult::Match) => Ok(true),
        Ok(VerifyResult::Mismatch { .. } | VerifyResult::Inconclusive { .. }) => Ok(false),
        Err(error) => Err(error),
    }
}

async fn destination_holds_source_bytes(
    src_backend: &dyn BackendAdapter,
    src: &ObjectRef,
    dst_backend: &dyn BackendAdapter,
    dst: &ObjectRef,
    total_len: u64,
) -> bool {
    matches!(
        verified_destination_match(src_backend, src, dst_backend, dst, total_len).await,
        Ok(true)
    )
}

fn persist_multipart_session(
    journal: &MultipartJournal<'_>,
    session: &MultipartUploadSession,
    cleanup_pending: bool,
) -> Result<(), BackendError> {
    journal
        .job_store
        .save_multipart_upload(&PersistedMultipartUpload {
            job_id: journal.job_id.clone(),
            entry_index: journal.entry_index,
            destination_backend: session.destination.key.backend_id.0.clone(),
            destination_path: session.destination.display_path.clone(),
            source_identity: journal.source_identity.clone(),
            upload_id: session.upload_id.clone(),
            total_len: journal.total_len,
            part_size: journal.part_size,
            cleanup_pending,
        })
        .map_err(jobstore_error_to_backend_error)
}

fn multipart_session_from_persisted(upload: &PersistedMultipartUpload) -> MultipartUploadSession {
    MultipartUploadSession {
        upload_id: upload.upload_id.clone(),
        destination: ObjectRef {
            key: ObjectKey {
                backend_id: BackendId(upload.destination_backend.clone()),
                stable_id: StableId(upload.upload_id.clone()),
            },
            parent_stable_id: None,
            display_path: upload.destination_path.clone(),
        },
    }
}

pub(crate) async fn copy_chunked_with_range_writes(
    request: ChunkedRangeWriteRequest<'_>,
) -> Result<(), BackendError> {
    let ChunkedRangeWriteRequest {
        src_backend,
        src,
        dst_backend,
        dst,
        total_len,
        requested_in_flight,
        recorder,
        progress_tx,
        pause_rx,
        cancel_token,
        retry_policy,
    } = request;
    let plan = plan_chunked_transfer(total_len, CHUNKED_RANGE_CHUNK_BYTES, requested_in_flight);
    let _request_budget = request_budget(plan.max_in_flight).await?;
    let retry = ChunkRetryControls {
        policy: retry_policy,
        pause_rx: pause_rx.clone(),
        cancel_token: cancel_token.clone(),
    };
    let completed_offsets = resumable_completed_offsets(
        src_backend,
        &src,
        dst_backend,
        dst,
        &plan,
        recorder.as_ref(),
    )
    .await?;

    if let Some(recorder) = &recorder {
        recorder.record_pending_ranges(&plan, &completed_offsets)?;
        recorder
            .job_store
            .flush()
            .map_err(jobstore_error_to_backend_error)?;
    }

    let mut completed_bytes = plan
        .ranges
        .iter()
        .filter(|range| completed_offsets.contains(&range.offset))
        .map(|range| range.length.unwrap_or(0))
        .sum::<u64>();
    if completed_bytes > 0 {
        let _ = progress_tx.send(ProgressInfo::single_file(
            completed_bytes,
            Some(total_len),
            false,
        ));
    }

    let mut next_to_schedule = 0usize;
    let mut in_flight = FuturesUnordered::<ChunkFuture<'_>>::new();
    loop {
        wait_for_resume_or_cancel(pause_rx.clone(), cancel_token.clone()).await?;

        while in_flight.len() < plan.max_in_flight && next_to_schedule < plan.ranges.len() {
            let range = plan.ranges[next_to_schedule].clone();
            let index = next_to_schedule;
            next_to_schedule += 1;
            if completed_offsets.contains(&range.offset) {
                continue;
            }
            in_flight.push(read_range_chunk(
                src_backend,
                src.clone(),
                index,
                range,
                recorder.clone(),
                retry.clone(),
            ));
        }

        let Some(chunk) = in_flight.next().await else {
            break;
        };
        let chunk = chunk?;
        wait_for_resume_or_cancel(pause_rx.clone(), cancel_token.clone()).await?;
        let write_bytes: Arc<[u8]> = chunk.bytes.into();
        let write_result = retry_backend_operation(
            &retry.policy,
            retry.pause_rx.clone(),
            retry.cancel_token.clone(),
            || {
                let reader = Cursor::new(Arc::clone(&write_bytes));
                dst_backend.write_range_from_reader(
                    dst,
                    Box::pin(reader),
                    chunk.range.clone(),
                    total_len,
                )
            },
        )
        .await;
        match write_result {
            Ok(_) => {
                if let Some(recorder) = &recorder {
                    recorder.record(&chunk.range, PersistedChunkState::Completed, None)?;
                }
                completed_bytes = completed_bytes.saturating_add(chunk.range.length.unwrap_or(0));
                let _ = progress_tx.send(ProgressInfo::single_file(
                    completed_bytes,
                    Some(total_len),
                    false,
                ));
            }
            Err(error) => {
                if let Some(recorder) = &recorder {
                    recorder.record(&chunk.range, PersistedChunkState::Failed, None)?;
                    recorder
                        .job_store
                        .flush()
                        .map_err(jobstore_error_to_backend_error)?;
                }
                return Err(error);
            }
        }
    }

    if let Some(recorder) = &recorder {
        recorder
            .job_store
            .flush()
            .map_err(jobstore_error_to_backend_error)?;
    }
    if completed_bytes != total_len {
        return Err(BackendError::Internal(Box::new(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!(
                "chunked range copy completed {completed_bytes} bytes for expected {total_len}"
            ),
        ))));
    }

    Ok(())
}

pub(crate) async fn read_chunked_to_vec(
    backend: &dyn BackendAdapter,
    object: ObjectRef,
    total_len: u64,
    requested_in_flight: usize,
    recorder: Option<ChunkStateRecorder>,
    retry: ChunkRetryControls,
) -> Result<Vec<u8>, BackendError> {
    let capacity = usize::try_from(total_len.min(CHUNKED_RANGE_MAX_BUFFERED_BYTES)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    let plan = plan_chunked_transfer(total_len, CHUNKED_RANGE_CHUNK_BYTES, requested_in_flight);
    let _request_budget = request_budget(plan.max_in_flight).await?;
    if let Some(recorder) = &recorder {
        recorder.record_plan(&plan)?;
    }
    let mut reader = ChunkedRangeReader::with_plan(backend, object, plan, recorder.clone(), retry);
    if let Err(error) = reader.read_to_end(&mut bytes).await {
        if let Some(recorder) = &recorder {
            recorder
                .job_store
                .flush()
                .map_err(jobstore_error_to_backend_error)?;
        }
        return Err(BackendError::Internal(Box::new(error)));
    }
    if bytes.len() as u64 != total_len {
        return Err(BackendError::Internal(Box::new(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!(
                "chunked range read returned {} bytes for expected {total_len}",
                bytes.len()
            ),
        ))));
    }
    Ok(bytes)
}

pub(crate) fn plan_chunked_transfer(
    total_len: u64,
    chunk_len: u64,
    requested_in_flight: usize,
) -> ChunkedTransferPlan {
    let chunk_len = chunk_len.max(1);
    let max_in_flight = requested_in_flight.clamp(1, CHUNKED_RANGE_MAX_IN_FLIGHT);
    let mut ranges = Vec::new();
    let mut offset = 0;
    while offset < total_len {
        let len = (total_len - offset).min(chunk_len);
        ranges.push(ByteRange {
            offset,
            length: Some(len),
        });
        offset += len;
    }
    ChunkedTransferPlan {
        max_in_flight,
        ranges,
    }
}

fn read_range_chunk<'a>(
    backend: &'a dyn BackendAdapter,
    object: ObjectRef,
    index: usize,
    range: ByteRange,
    recorder: Option<ChunkStateRecorder>,
    retry: ChunkRetryControls,
) -> ChunkFuture<'a> {
    Box::pin(async move {
        if let Some(recorder) = &recorder {
            recorder.record(&range, PersistedChunkState::Running, None)?;
        }
        let expected_len = range
            .length
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(BackendError::Unsupported {
                operation: TRANSFER_MEMORY_BUDGET_EXCEEDED_OPERATION,
            })?;
        let result = retry_backend_operation(
            &retry.policy,
            retry.pause_rx,
            retry.cancel_token,
            || async {
                let permit = MemoryBudget::global().acquire(expected_len).await.ok_or(
                    BackendError::Unsupported {
                        operation: TRANSFER_MEMORY_BUDGET_EXCEEDED_OPERATION,
                    },
                )?;
                let mut handle = backend.read(&object, Some(range.clone())).await?;
                let mut bytes = Vec::with_capacity(expected_len);
                handle
                    .stream
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|error| stream_read_error_to_backend_error(&object, error))?;
                if bytes.len() != expected_len {
                    return Err(stream_read_error_to_backend_error(
                        &object,
                        std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            format!(
                                "range returned {} bytes for expected {expected_len}",
                                bytes.len()
                            ),
                        ),
                    ));
                }
                Ok((bytes, permit))
            },
        )
        .await;

        match result {
            Ok((bytes, permit)) => Ok(RangeChunk {
                index,
                range,
                bytes,
                _permit: permit,
            }),
            Err(error) => {
                if let Some(recorder) = &recorder {
                    recorder.record(&range, PersistedChunkState::Failed, None)?;
                }
                Err(error)
            }
        }
    })
}

async fn retry_backend_operation<T, Operation, OperationFuture>(
    policy: &RetryPolicy,
    pause_rx: watch::Receiver<bool>,
    cancel_token: CancellationToken,
    mut operation: Operation,
) -> Result<T, BackendError>
where
    Operation: FnMut() -> OperationFuture,
    OperationFuture: Future<Output = Result<T, BackendError>>,
{
    let mut attempt = 0;
    loop {
        wait_for_resume_or_cancel(pause_rx.clone(), cancel_token.clone()).await?;
        let result = tokio::select! {
            result = operation() => result,
            () = cancel_token.cancelled() => return transfer_canceled(),
        };
        match result {
            Ok(value) => return Ok(value),
            Err(error) if policy.should_retry(attempt, &error) => {
                let delay = policy.delay_for_attempt(attempt);
                attempt += 1;
                tokio::select! {
                    () = sleep(delay) => {}
                    () = cancel_token.cancelled() => return transfer_canceled(),
                }
            }
            Err(error) => return Err(error),
        }
    }
}

fn stream_read_error_to_backend_error(object: &ObjectRef, error: std::io::Error) -> BackendError {
    let backend_kind = object
        .key
        .backend_id
        .0
        .split_once(':')
        .map_or(object.key.backend_id.0.as_str(), |(kind, _)| kind);
    if matches!(backend_kind, "s3" | "sftp" | "smb" | "pcloud") {
        BackendError::Network(NetworkErrorDetail {
            message: format!("range stream read failed: {error}"),
            is_timeout: error.kind() == std::io::ErrorKind::TimedOut,
        })
    } else {
        BackendError::Internal(Box::new(error))
    }
}

fn jobstore_error_to_backend_error(error: JobStoreError) -> BackendError {
    BackendError::Internal(Box::new(std::io::Error::other(error.to_string())))
}

async fn resumable_completed_offsets(
    src_backend: &dyn BackendAdapter,
    src: &ObjectRef,
    dst_backend: &dyn BackendAdapter,
    dst: &ObjectRef,
    plan: &ChunkedTransferPlan,
    recorder: Option<&ChunkStateRecorder>,
) -> Result<HashSet<u64>, BackendError> {
    let Some(recorder) = recorder else {
        return Ok(HashSet::new());
    };
    let Some(destination_size) = dst_backend
        .stat(dst)
        .await
        .ok()
        .and_then(|metadata| (!metadata.is_dir).then_some(metadata.size).flatten())
    else {
        return Ok(HashSet::new());
    };
    let persisted = recorder.completed_offsets(plan)?;
    let mut completed = HashSet::new();
    for range in &plan.ranges {
        let length = range.length.unwrap_or(0);
        if !persisted.contains(&range.offset)
            || range.offset.saturating_add(length) > destination_size
        {
            break;
        }
        completed.insert(range.offset);
    }
    let Some(last_offset) = completed.iter().copied().max() else {
        return Ok(completed);
    };
    let Some(last_range) = plan
        .ranges
        .iter()
        .find(|range| range.offset == last_offset)
        .cloned()
    else {
        return Ok(HashSet::new());
    };
    let source = read_backend_range(src_backend, src, last_range.clone()).await?;
    let destination = read_backend_range(dst_backend, dst, last_range).await?;
    if source != destination {
        completed.remove(&last_offset);
    }
    Ok(completed)
}

async fn read_backend_range(
    backend: &dyn BackendAdapter,
    object: &ObjectRef,
    range: ByteRange,
) -> Result<Vec<u8>, BackendError> {
    let handle = backend.read(object, Some(range)).await?;
    let mut reader = handle.stream;
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| BackendError::Internal(Box::new(error)))?;
    Ok(bytes)
}

async fn wait_for_resume_or_cancel(
    mut pause_rx: watch::Receiver<bool>,
    cancel_token: CancellationToken,
) -> Result<(), BackendError> {
    loop {
        if cancel_token.is_cancelled() {
            return transfer_canceled();
        }
        if !*pause_rx.borrow() {
            return Ok(());
        }
        tokio::select! {
            changed = pause_rx.changed() => changed.map_err(|_| {
                BackendError::Internal(Box::new(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "pause control channel closed",
                )))
            })?,
            () = cancel_token.cancelled() => return transfer_canceled(),
        }
    }
}

fn transfer_canceled<T>() -> Result<T, BackendError> {
    Err(BackendError::Internal(Box::new(std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "transfer canceled",
    ))))
}

/// Holds `count` slots of the process-wide budget of in-flight transfer requests.
async fn request_budget(count: usize) -> Result<tokio::sync::OwnedSemaphorePermit, BackendError> {
    global_concurrency_governor()
        .acquire_in_flight(count)
        .await
        .ok_or_else(|| {
            BackendError::Internal(Box::new(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "global transfer request budget closed",
            )))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn plan_chunked_transfer_splits_tail_range() {
        let plan = plan_chunked_transfer(10, 4, 3);

        assert_eq!(plan.max_in_flight, 3);
        assert_eq!(plan.ranges.len(), 3);
        assert_eq!(plan.ranges[0].offset, 0);
        assert_eq!(plan.ranges[0].length, Some(4));
        assert_eq!(plan.ranges[1].offset, 4);
        assert_eq!(plan.ranges[1].length, Some(4));
        assert_eq!(plan.ranges[2].offset, 8);
        assert_eq!(plan.ranges[2].length, Some(2));

        let capped = plan_chunked_transfer(10, 4, CHUNKED_RANGE_MAX_IN_FLIGHT + 1);
        assert_eq!(capped.max_in_flight, CHUNKED_RANGE_MAX_IN_FLIGHT);
    }

    #[test]
    fn should_use_chunked_range_requires_size_and_capability() {
        assert!(should_use_chunked_range(
            Some(CHUNKED_RANGE_MIN_BYTES),
            true
        ));
        assert!(!should_use_chunked_range(
            Some(CHUNKED_RANGE_MIN_BYTES - 1),
            true
        ));
        assert!(!should_use_chunked_range(
            Some(CHUNKED_RANGE_MAX_BUFFERED_BYTES + 1),
            true
        ));
        assert!(!should_use_chunked_range(
            Some(CHUNKED_RANGE_MIN_BYTES),
            false
        ));
        assert!(!should_use_chunked_range(None, true));
    }

    #[tokio::test]
    async fn retry_wait_exits_when_transfer_is_canceled_while_paused() {
        let (_pause_tx, pause_rx) = watch::channel(true);
        let cancel_token = CancellationToken::new();
        let cancel = cancel_token.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            cancel.cancel();
        });
        let calls = AtomicU32::new(0);

        let result: Result<(), BackendError> =
            retry_backend_operation(&RetryPolicy::default(), pause_rx, cancel_token, || {
                calls.fetch_add(1, Ordering::Relaxed);
                async { Ok(()) }
            })
            .await;

        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn retry_backoff_exits_immediately_after_cancellation() {
        let (_pause_tx, pause_rx) = watch::channel(false);
        let cancel_token = CancellationToken::new();
        let cancel = cancel_token.clone();
        let calls = AtomicU32::new(0);
        let policy = RetryPolicy {
            max_retries: 5,
            initial_delay: std::time::Duration::from_secs(60),
            max_delay: std::time::Duration::from_secs(60),
            backoff_factor: 2.0,
        };

        let result: Result<(), BackendError> =
            retry_backend_operation(&policy, pause_rx, cancel_token, || {
                calls.fetch_add(1, Ordering::Relaxed);
                cancel.cancel();
                async {
                    Err(BackendError::Network(NetworkErrorDetail {
                        message: "injected disconnect".to_string(),
                        is_timeout: false,
                    }))
                }
            })
            .await;

        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn retry_cancels_an_in_flight_backend_operation() {
        let (_pause_tx, pause_rx) = watch::channel(false);
        let cancel_token = CancellationToken::new();
        let cancel = cancel_token.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            cancel.cancel();
        });

        let result: Result<(), BackendError> =
            retry_backend_operation(&RetryPolicy::default(), pause_rx, cancel_token, || {
                std::future::pending()
            })
            .await;

        assert!(result.is_err());
    }
}
