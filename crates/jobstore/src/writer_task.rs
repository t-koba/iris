use crate::repository::{
    JobStoreError, JobStoreRepository, PersistedChunkStateRow, PersistedJob,
    PersistedMultipartUpload,
};
use iris_core_domain::types::transfer::{JobId, JobState, TaskId, TaskState};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::warn;

const WRITE_BATCH_WINDOW: Duration = Duration::from_millis(16);
const WRITE_CHANNEL_CAPACITY: usize = 1024;

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
        Ok(Self {
            reader_db_path: db_path.to_path_buf(),
            writer: JobStoreWriterTask::open(db_path)?,
        })
    }

    pub fn open_in_memory() -> Result<Self, JobStoreError> {
        let unique = std::env::temp_dir().join(format!(
            "iris-jobstore-{}.sqlite3",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        Ok(Self {
            reader_db_path: unique.clone(),
            writer: JobStoreWriterTask::open(&unique)?,
        })
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
    fn open(db_path: &Path) -> Result<Self, JobStoreError> {
        Self::spawn(JobStoreRepository::open(db_path)?)
    }

    fn spawn(repository: JobStoreRepository) -> Result<Self, JobStoreError> {
        let (tx, rx) = mpsc::sync_channel(WRITE_CHANNEL_CAPACITY);
        let join_handle = thread::Builder::new()
            .name("iris-jobstore-writer".to_string())
            .spawn(move || writer_loop(repository, rx))
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

#[tracing::instrument(skip(repository, rx))]
fn writer_loop(repository: JobStoreRepository, rx: Receiver<WriteCommand>) {
    while let Ok(first_command) = rx.recv() {
        let mut batch = vec![first_command];
        let mut shutdown_requested = false;

        loop {
            match rx.recv_timeout(WRITE_BATCH_WINDOW) {
                Ok(WriteCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                    shutdown_requested = true;
                    break;
                }
                Ok(command) => batch.push(command),
                Err(RecvTimeoutError::Timeout) => break,
            }
        }

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
