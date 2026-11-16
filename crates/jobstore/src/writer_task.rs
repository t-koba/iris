use crate::repository::{
    JobStoreError, JobStoreRepository, PersistedChunkStateRow, PersistedJob,
    PersistedMultipartUpload, PersistedRangeSession,
};
use iris_core_domain::types::transfer::{JobId, JobState, TaskId, TaskState};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::warn;

const WRITE_CHANNEL_CAPACITY: usize = 1024;

/// Bounds collection of each writer batch, measured from its first command.
///
/// The default is a 16 ms window and at most 1024 commands. Store constructors
/// reject zero windows, zero command limits, and windows that overflow `Instant`.
#[derive(Clone, Copy, Debug)]
pub struct WriterBatchPolicy {
    /// Maximum collection time from receipt of the first command.
    pub window: Duration,
    /// Maximum commands per batch, including barriers and the first command.
    pub max_commands: usize,
}

impl Default for WriterBatchPolicy {
    fn default() -> Self {
        Self {
            window: Duration::from_millis(16),
            max_commands: 1024,
        }
    }
}

impl WriterBatchPolicy {
    fn validate(self) -> Result<(), JobStoreError> {
        if self.window.is_zero()
            || self.max_commands == 0
            || Instant::now().checked_add(self.window).is_none()
        {
            return Err(JobStoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "writer batch policy requires a nonzero, representable window and command limit",
            )));
        }
        Ok(())
    }
}

enum WriteCommand {
    SaveJob {
        job: PersistedJob,
        ack: mpsc::Sender<Result<(), JobStoreError>>,
    },
    UpdateTaskProgress {
        task_id: TaskId,
        bytes_completed: u64,
    },
    UpdateTaskState {
        task_id: TaskId,
        state: TaskState,
        error_message: Option<String>,
    },
    UpdateJobState {
        job_id: JobId,
        state: JobState,
        updated_at: String,
    },
    UpsertChunkState {
        chunk: PersistedChunkStateRow,
    },
    SaveRangeSession {
        session: PersistedRangeSession,
        ack: mpsc::Sender<Result<(), JobStoreError>>,
    },
    BindRangeFileIdentity {
        session: PersistedRangeSession,
        identity: String,
        ack: mpsc::Sender<Result<(), JobStoreError>>,
    },
    PersistRangeChunkChecksums {
        session: PersistedRangeSession,
        ranges: Vec<(u64, u64)>,
        ack: mpsc::Sender<Result<(), JobStoreError>>,
    },
    RemoveRangeSession {
        job_id: JobId,
        entry_index: u64,
        expected: Option<PersistedRangeSession>,
        ack: mpsc::Sender<Result<(), JobStoreError>>,
    },
    SaveMultipartUpload {
        upload: PersistedMultipartUpload,
        ack: mpsc::Sender<Result<(), JobStoreError>>,
    },
    RemoveMultipartUpload {
        job_id: JobId,
        entry_index: u64,
        ack: mpsc::Sender<Result<(), JobStoreError>>,
    },
    Flush {
        ack: mpsc::Sender<Result<(), JobStoreError>>,
    },
    Shutdown,
}

pub struct JobStore {
    reader_db_path: std::path::PathBuf,
    writer: JobStoreWriterTask,
}

impl JobStore {
    pub fn open(db_path: &Path) -> Result<Self, JobStoreError> {
        Self::open_with_policy(db_path, WriterBatchPolicy::default())
    }

    /// Open a store with bounded writer batches. Invalid policies return an
    /// `InvalidInput` I/O error before opening the database.
    pub fn open_with_policy(
        db_path: &Path,
        policy: WriterBatchPolicy,
    ) -> Result<Self, JobStoreError> {
        policy.validate()?;
        Ok(Self {
            reader_db_path: db_path.to_path_buf(),
            writer: JobStoreWriterTask::open(db_path, policy)?,
        })
    }

    pub fn open_in_memory() -> Result<Self, JobStoreError> {
        Self::open_in_memory_with_policy(WriterBatchPolicy::default())
    }

    /// Like `open_in_memory`, with the policy validation of `open_with_policy`.
    pub fn open_in_memory_with_policy(policy: WriterBatchPolicy) -> Result<Self, JobStoreError> {
        let unique = std::env::temp_dir().join(format!(
            "iris-jobstore-{}.sqlite3",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        Self::open_with_policy(&unique, policy)
    }

    pub fn save_job(&self, job: &PersistedJob) -> Result<(), JobStoreError> {
        self.writer.save_job(job.clone())
    }

    pub fn load_job(&self, job_id: &JobId) -> Result<Option<PersistedJob>, JobStoreError> {
        self.with_reader(|reader| reader.load_job(job_id))
    }

    pub fn load_all_incomplete_jobs(&self) -> Result<Vec<PersistedJob>, JobStoreError> {
        self.with_reader(JobStoreRepository::load_all_incomplete_jobs)
    }

    pub fn update_task_progress(
        &self,
        task_id: &TaskId,
        bytes_completed: u64,
    ) -> Result<(), JobStoreError> {
        self.writer
            .update_task_progress(task_id.clone(), bytes_completed)
    }

    pub fn update_task_state(
        &self,
        task_id: &TaskId,
        state: &TaskState,
        error_message: Option<&str>,
    ) -> Result<(), JobStoreError> {
        self.writer.update_task_state(
            task_id.clone(),
            state.clone(),
            error_message.map(str::to_string),
        )
    }

    pub fn update_job_state(
        &self,
        job_id: &JobId,
        state: &JobState,
        updated_at: &str,
    ) -> Result<(), JobStoreError> {
        self.writer
            .update_job_state(job_id.clone(), state.clone(), updated_at.to_string())
    }

    pub fn upsert_chunk_state(&self, chunk: &PersistedChunkStateRow) -> Result<(), JobStoreError> {
        self.writer.upsert_chunk_state(chunk.clone())
    }

    pub fn flush(&self) -> Result<(), JobStoreError> {
        self.writer.flush()
    }

    pub fn load_chunk_states(
        &self,
        job_id: &JobId,
    ) -> Result<Vec<PersistedChunkStateRow>, JobStoreError> {
        self.with_reader(|reader| reader.load_chunk_states(job_id))
    }

    pub fn save_range_session(&self, session: &PersistedRangeSession) -> Result<(), JobStoreError> {
        self.writer
            .send_with_ack(|ack| WriteCommand::SaveRangeSession {
                session: session.clone(),
                ack,
            })
    }

    pub fn bind_range_file_identity(
        &self,
        session: &PersistedRangeSession,
        identity: &str,
    ) -> Result<(), JobStoreError> {
        self.writer
            .send_with_ack(|ack| WriteCommand::BindRangeFileIdentity {
                session: session.clone(),
                identity: identity.into(),
                ack,
            })
    }

    /// Commit the complete checksum set for this staging session with FULL
    /// synchronous durability before its data is installed at the final path.
    pub fn persist_range_chunk_checksums(
        &self,
        session: &PersistedRangeSession,
        ranges: &[(u64, u64)],
    ) -> Result<(), JobStoreError> {
        self.writer
            .send_with_ack(|ack| WriteCommand::PersistRangeChunkChecksums {
                session: session.clone(),
                ranges: ranges.to_vec(),
                ack,
            })
    }

    pub fn load_range_session(
        &self,
        job_id: &JobId,
        entry_index: u64,
    ) -> Result<Option<PersistedRangeSession>, JobStoreError> {
        self.with_reader(|reader| reader.load_range_session(job_id, entry_index))
    }

    pub fn remove_range_session(
        &self,
        job_id: &JobId,
        entry_index: u64,
    ) -> Result<(), JobStoreError> {
        self.writer
            .send_with_ack(|ack| WriteCommand::RemoveRangeSession {
                job_id: job_id.clone(),
                entry_index,
                expected: None,
                ack,
            })
    }

    pub fn discard_range_session(
        &self,
        session: &PersistedRangeSession,
    ) -> Result<(), JobStoreError> {
        self.writer
            .send_with_ack(|ack| WriteCommand::RemoveRangeSession {
                job_id: session.job_id.clone(),
                entry_index: session.entry_index,
                expected: Some(session.clone()),
                ack,
            })
    }

    pub fn save_multipart_upload(
        &self,
        upload: &PersistedMultipartUpload,
    ) -> Result<(), JobStoreError> {
        self.writer.save_multipart_upload(upload.clone())
    }

    pub fn load_multipart_upload(
        &self,
        job_id: &JobId,
        entry_index: u64,
    ) -> Result<Option<PersistedMultipartUpload>, JobStoreError> {
        self.with_reader(|reader| reader.load_multipart_upload(job_id, entry_index))
    }

    pub fn remove_multipart_upload(
        &self,
        job_id: &JobId,
        entry_index: u64,
    ) -> Result<(), JobStoreError> {
        self.writer
            .remove_multipart_upload(job_id.clone(), entry_index)
    }

    fn with_reader<T>(
        &self,
        read: impl FnOnce(&JobStoreRepository) -> Result<T, JobStoreError>,
    ) -> Result<T, JobStoreError> {
        let reader = JobStoreRepository::open(&self.reader_db_path)?;
        read(&reader)
    }
}

struct JobStoreWriterTask {
    tx: SyncSender<WriteCommand>,
    join_handle: Mutex<Option<JoinHandle<()>>>,
}

impl JobStoreWriterTask {
    fn open(db_path: &Path, policy: WriterBatchPolicy) -> Result<Self, JobStoreError> {
        Self::spawn(JobStoreRepository::open(db_path)?, policy)
    }

    fn spawn(
        repository: JobStoreRepository,
        policy: WriterBatchPolicy,
    ) -> Result<Self, JobStoreError> {
        let (tx, rx) = mpsc::sync_channel(WRITE_CHANNEL_CAPACITY);
        let join_handle = thread::Builder::new()
            .name("iris-jobstore-writer".to_string())
            .spawn(move || writer_loop(repository, rx, policy))
            .map_err(JobStoreError::Io)?;
        Ok(Self {
            tx,
            join_handle: Mutex::new(Some(join_handle)),
        })
    }

    fn save_job(&self, job: PersistedJob) -> Result<(), JobStoreError> {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.send(WriteCommand::SaveJob { job, ack: ack_tx })?;
        ack_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(writer_channel_error)?
    }

    fn update_task_progress(
        &self,
        task_id: TaskId,
        bytes_completed: u64,
    ) -> Result<(), JobStoreError> {
        self.send(WriteCommand::UpdateTaskProgress {
            task_id,
            bytes_completed,
        })
    }

    fn update_task_state(
        &self,
        task_id: TaskId,
        state: TaskState,
        error_message: Option<String>,
    ) -> Result<(), JobStoreError> {
        self.send(WriteCommand::UpdateTaskState {
            task_id,
            state,
            error_message,
        })
    }

    fn update_job_state(
        &self,
        job_id: JobId,
        state: JobState,
        updated_at: String,
    ) -> Result<(), JobStoreError> {
        self.send(WriteCommand::UpdateJobState {
            job_id,
            state,
            updated_at,
        })
    }

    fn upsert_chunk_state(&self, chunk: PersistedChunkStateRow) -> Result<(), JobStoreError> {
        self.send(WriteCommand::UpsertChunkState { chunk })
    }

    fn save_multipart_upload(&self, upload: PersistedMultipartUpload) -> Result<(), JobStoreError> {
        self.send_with_ack(|ack| WriteCommand::SaveMultipartUpload { upload, ack })
    }

    fn remove_multipart_upload(
        &self,
        job_id: JobId,
        entry_index: u64,
    ) -> Result<(), JobStoreError> {
        self.send_with_ack(|ack| WriteCommand::RemoveMultipartUpload {
            job_id,
            entry_index,
            ack,
        })
    }

    fn flush(&self) -> Result<(), JobStoreError> {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.send(WriteCommand::Flush { ack: ack_tx })?;
        ack_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(writer_channel_error)?
    }

    fn send(&self, command: WriteCommand) -> Result<(), JobStoreError> {
        self.tx.send(command).map_err(writer_channel_error)
    }

    fn send_with_ack(
        &self,
        command: impl FnOnce(mpsc::Sender<Result<(), JobStoreError>>) -> WriteCommand,
    ) -> Result<(), JobStoreError> {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.send(command(ack_tx))?;
        ack_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(writer_channel_error)?
    }
}

impl Drop for JobStoreWriterTask {
    fn drop(&mut self) {
        let _ = self.tx.send(WriteCommand::Shutdown);
        let _ = self
            .join_handle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }
}

// The first command counts toward the limit, including when it is Shutdown.
fn collect_batch(
    first_command: WriteCommand,
    rx: &Receiver<WriteCommand>,
    policy: WriterBatchPolicy,
) -> (Vec<WriteCommand>, bool) {
    let deadline = Instant::now() + policy.window;
    let mut shutdown_requested = matches!(first_command, WriteCommand::Shutdown);
    let mut batch = vec![first_command];
    while !shutdown_requested && batch.len() < policy.max_commands {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(command) => {
                shutdown_requested = matches!(command, WriteCommand::Shutdown);
                batch.push(command);
            }
            Err(RecvTimeoutError::Disconnected) => {
                shutdown_requested = true;
                break;
            }
            Err(RecvTimeoutError::Timeout) => break,
        }
    }
    (batch, shutdown_requested)
}

#[tracing::instrument(skip(repository, rx))]
fn writer_loop(
    repository: JobStoreRepository,
    rx: Receiver<WriteCommand>,
    policy: WriterBatchPolicy,
) {
    while let Ok(first_command) = rx.recv() {
        let (batch, shutdown_requested) = collect_batch(first_command, &rx, policy);

        let mut latest_task_progress = std::collections::HashMap::<TaskId, usize>::new();
        let mut latest_job_state = std::collections::HashMap::<JobId, usize>::new();
        let mut latest_chunk_state = std::collections::HashMap::<(JobId, u64, u64), usize>::new();
        let mut skipped = vec![false; batch.len()];

        for (index, command) in batch.iter().enumerate() {
            match command {
                WriteCommand::UpdateTaskProgress { task_id, .. } => {
                    if let Some(previous) = latest_task_progress.insert(task_id.clone(), index) {
                        skipped[previous] = true;
                    }
                }
                WriteCommand::UpdateJobState { job_id, .. } => {
                    if let Some(previous) = latest_job_state.insert(job_id.clone(), index) {
                        skipped[previous] = true;
                    }
                }
                WriteCommand::UpsertChunkState { chunk } => {
                    let key = (chunk.job_id.clone(), chunk.entry_index, chunk.offset);
                    if let Some(previous) = latest_chunk_state.insert(key, index) {
                        skipped[previous] = true;
                    }
                }
                _ => {}
            }
        }

        for (index, command) in batch.into_iter().enumerate() {
            if skipped[index] {
                continue;
            }
            if let Err(error) = apply_command(&repository, command) {
                warn!("jobstore writer task command failed: {error}");
            }
        }

        if shutdown_requested {
            break;
        }
    }
}

fn apply_command(
    repository: &JobStoreRepository,
    command: WriteCommand,
) -> Result<(), JobStoreError> {
    match command {
        WriteCommand::SaveJob { job, ack } => {
            let result = repository.save_job(&job);
            let send_result = result.as_ref().map(|_| ()).map_err(writer_result_clone);
            let _ = ack.send(send_result);
            result
        }
        WriteCommand::UpdateTaskProgress {
            task_id,
            bytes_completed,
        } => repository.update_task_progress(&task_id, bytes_completed),
        WriteCommand::UpdateTaskState {
            task_id,
            state,
            error_message,
        } => repository.update_task_state(&task_id, &state, error_message.as_deref()),
        WriteCommand::UpdateJobState {
            job_id,
            state,
            updated_at,
        } => repository.update_job_state(&job_id, &state, &updated_at),
        WriteCommand::UpsertChunkState { chunk } => repository.upsert_chunk_state(&chunk),
        WriteCommand::SaveRangeSession { session, ack } => {
            send_repository_result(ack, repository.save_range_session(&session))
        }
        WriteCommand::BindRangeFileIdentity {
            session,
            identity,
            ack,
        } => send_repository_result(
            ack,
            repository.bind_range_file_identity(&session, &identity),
        ),
        WriteCommand::PersistRangeChunkChecksums {
            session,
            ranges,
            ack,
        } => send_repository_result(
            ack,
            repository.persist_range_chunk_checksums(&session, &ranges),
        ),
        WriteCommand::RemoveRangeSession {
            job_id,
            entry_index,
            expected,
            ack,
        } => send_repository_result(
            ack,
            match expected {
                Some(session) => repository.discard_range_session(&session),
                None => repository.remove_range_session(&job_id, entry_index),
            },
        ),
        WriteCommand::SaveMultipartUpload { upload, ack } => {
            send_repository_result(ack, repository.save_multipart_upload(&upload))
        }
        WriteCommand::RemoveMultipartUpload {
            job_id,
            entry_index,
            ack,
        } => send_repository_result(
            ack,
            repository.remove_multipart_upload(&job_id, entry_index),
        ),
        WriteCommand::Flush { ack } => {
            let _ = ack.send(Ok(()));
            Ok(())
        }
        WriteCommand::Shutdown => Ok(()),
    }
}

fn send_repository_result(
    ack: mpsc::Sender<Result<(), JobStoreError>>,
    result: Result<(), JobStoreError>,
) -> Result<(), JobStoreError> {
    let response = result.as_ref().map(|_| ()).map_err(writer_result_clone);
    let _ = ack.send(response);
    result
}

fn writer_channel_error<T>(error: T) -> JobStoreError
where
    T: std::fmt::Display,
{
    JobStoreError::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        error.to_string(),
    ))
}

fn writer_result_clone(error: &JobStoreError) -> JobStoreError {
    match error {
        JobStoreError::Database(inner) => {
            JobStoreError::Io(std::io::Error::other(inner.to_string()))
        }
        JobStoreError::Io(inner) => {
            JobStoreError::Io(std::io::Error::new(inner.kind(), inner.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{
        PersistedChunkState, PersistedChunkStateRow, PersistedJob, PersistedTask,
    };
    use iris_core_domain::types::transfer::{JobId, JobState, TaskId, TaskKind};

    fn sample_job() -> PersistedJob {
        PersistedJob {
            id: JobId(1),
            state: JobState::Queued,
            created_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
            description: "sample".into(),
            tasks: vec![PersistedTask {
                id: TaskId(7),
                job_id: JobId(1),
                kind: TaskKind::Copy,
                state: TaskState::Pending,
                src_backend: Some("local".into()),
                src_stable_id: Some("src".into()),
                src_display_path: Some("/tmp/src".into()),
                dst_backend: Some("local".into()),
                dst_stable_id: Some("dst".into()),
                dst_display_path: Some("/tmp/dst".into()),
                bytes_total: 1024,
                bytes_completed: 0,
                error_message: None,
            }],
        }
    }

    fn progress(bytes_completed: u64) -> WriteCommand {
        WriteCommand::UpdateTaskProgress {
            task_id: TaskId(7),
            bytes_completed,
        }
    }

    #[test]
    fn invalid_batch_policies_are_rejected() {
        for policy in [
            WriterBatchPolicy {
                window: Duration::ZERO,
                ..WriterBatchPolicy::default()
            },
            WriterBatchPolicy {
                max_commands: 0,
                ..WriterBatchPolicy::default()
            },
            WriterBatchPolicy {
                window: Duration::MAX,
                ..WriterBatchPolicy::default()
            },
        ] {
            assert!(matches!(
                JobStore::open_in_memory_with_policy(policy),
                Err(JobStoreError::Io(error))
                    if error.kind() == std::io::ErrorKind::InvalidInput
            ));
        }
    }

    #[test]
    fn batch_collection_caps_continuously_available_commands() {
        let (tx, rx) = mpsc::sync_channel(16);
        for value in 0..16 {
            tx.send(progress(value)).unwrap();
        }
        let policy = WriterBatchPolicy {
            window: Duration::from_millis(10),
            max_commands: 4,
        };
        let (batch, shutdown) = collect_batch(rx.recv().unwrap(), &rx, policy);
        assert!(!shutdown);
        assert!(!batch.is_empty());
        assert!(batch.len() <= policy.max_commands);
        let remaining = rx.try_iter().count();
        assert_eq!(batch.len() + remaining, 16);
        assert!(remaining >= 12);
    }

    #[test]
    fn batch_collection_stops_at_shutdown_or_disconnect() {
        let policy = WriterBatchPolicy {
            window: Duration::from_secs(5),
            ..WriterBatchPolicy::default()
        };
        for shutdown_first in [false, true] {
            let (tx, rx) = mpsc::sync_channel(3);
            if !shutdown_first {
                tx.send(progress(1)).unwrap();
            }
            tx.send(WriteCommand::Shutdown).unwrap();
            tx.send(progress(2)).unwrap();
            let (batch, shutdown) = collect_batch(rx.recv().unwrap(), &rx, policy);
            assert!(shutdown);
            assert_eq!(batch.len(), if shutdown_first { 1 } else { 2 });
            assert!(matches!(batch.last(), Some(WriteCommand::Shutdown)));
            assert_eq!(rx.try_iter().count(), 1);
        }

        let (tx, rx) = mpsc::sync_channel(2);
        tx.send(progress(1)).unwrap();
        tx.send(progress(2)).unwrap();
        drop(tx);
        let (batch, shutdown) = collect_batch(rx.recv().unwrap(), &rx, policy);
        assert!(shutdown);
        assert_eq!(batch.len(), 2);
    }

    #[test]
    fn flush_is_acknowledged_before_sustained_producer_finishes() {
        let directory = tempfile::tempdir().unwrap();
        let store = JobStore::open_with_policy(
            &directory.path().join("jobs.sqlite3"),
            WriterBatchPolicy {
                window: Duration::from_millis(25),
                // At one update per ms this cannot trigger within the ack bound:
                // the elapsed-time bound, not the size bound, must flush the batch.
                max_commands: 1024,
            },
        )
        .unwrap();
        store.save_job(&sample_job()).unwrap();
        let tx = store.writer.tx.clone();
        let (ack_tx, ack_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let producer = thread::spawn(move || {
            let start = Instant::now();
            tx.send(progress(1)).unwrap();
            tx.send(WriteCommand::Flush { ack: ack_tx }).unwrap();
            let mut value = 1;
            while start.elapsed() < Duration::from_millis(1500) {
                value += 1;
                tx.send(progress(value)).unwrap();
                thread::sleep(Duration::from_millis(1));
            }
            done_tx.send(()).unwrap();
            value
        });

        let ack = ack_rx.recv_timeout(Duration::from_millis(500));
        let still_sending = matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        // Join even on failure so no producer outlives the test or its database.
        let final_progress = producer.join().unwrap();
        ack.expect("flush ack within 500 ms of a 1500 ms producer run")
            .expect("successful flush");
        assert!(still_sending, "flush must not wait for producer idle");
        store.flush().unwrap();
        let loaded = store.load_job(&JobId(1)).unwrap().unwrap();
        assert_eq!(loaded.tasks[0].bytes_completed, final_progress);
    }

    #[test]
    fn writer_task_persists_job_and_progress() {
        let store = JobStore::open_in_memory().expect("open job store");
        let job = sample_job();
        store.save_job(&job).expect("save job");
        store
            .update_task_progress(&TaskId(7), 512)
            .expect("update progress");
        store.flush().expect("flush writer");

        let loaded = store
            .load_job(&JobId(1))
            .expect("load job")
            .expect("job should exist");
        assert_eq!(loaded.tasks[0].bytes_completed, 512);
    }

    #[test]
    fn writer_task_batches_chunk_state_updates() {
        let store = JobStore::open_in_memory().expect("open job store");
        let job = sample_job();
        store.save_job(&job).expect("save job");

        let mut chunk = PersistedChunkStateRow {
            job_id: JobId(1),
            entry_index: 0,
            offset: 0,
            len: 8 * 1024 * 1024,
            state: PersistedChunkState::Running,
            etag: None,
            checksum: None,
        };
        store
            .upsert_chunk_state(&chunk)
            .expect("upsert running chunk");
        chunk.state = PersistedChunkState::Completed;
        chunk.checksum = Some("sha256:done".to_string());
        store
            .upsert_chunk_state(&chunk)
            .expect("upsert completed chunk");
        store.flush().expect("flush writer");

        let chunks = store
            .load_chunk_states(&JobId(1))
            .expect("load chunk states");
        assert_eq!(chunks, vec![chunk]);
    }
}
