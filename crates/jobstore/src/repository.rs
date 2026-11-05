use crate::schema;
use iris_core_domain::types::transfer::{JobId, JobState, TaskId, TaskKind, TaskState};
use rusqlite::Connection;
use std::path::Path;
use tracing::debug;

#[derive(Debug, thiserror::Error)]
pub enum JobStoreError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug)]
pub struct PersistedJob {
    pub id: JobId,
    pub state: JobState,
    pub created_at: String,
    pub updated_at: String,
    pub description: String,
    pub tasks: Vec<PersistedTask>,
}

#[derive(Clone, Debug)]
pub struct PersistedTask {
    pub id: TaskId,
    pub job_id: JobId,
    pub kind: TaskKind,
    pub state: TaskState,
    pub src_backend: Option<String>,
    pub src_stable_id: Option<String>,
    pub src_display_path: Option<String>,
    pub dst_backend: Option<String>,
    pub dst_stable_id: Option<String>,
    pub dst_display_path: Option<String>,
    pub bytes_total: u64,
    pub bytes_completed: u64,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistedChunkState {
    Pending,
    Running,
    Completed,
    Failed,
}

/// Chunk state for resumable transfers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedChunkStateRow {
    pub job_id: JobId,
    pub entry_index: u64,
    pub offset: u64,
    pub len: u64,
    pub state: PersistedChunkState,
    pub etag: Option<String>,
    pub checksum: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedMultipartUpload {
    pub job_id: JobId,
    pub entry_index: u64,
    pub destination_backend: String,
    pub destination_path: String,
    pub source_identity: Option<String>,
    pub upload_id: String,
    pub total_len: u64,
    pub part_size: u64,
}

/// The job store is small; 16 MB of mmap is enough.
const JOBSTORE_MMAP_BYTES: u64 = 16 * 1024 * 1024;

/// SQLite job store; lets jobs be restored after an app restart.
pub struct JobStoreRepository {
    conn: Connection,
}

impl JobStoreRepository {
    pub fn open(db_path: &Path) -> Result<Self, JobStoreError> {
        Ok(Self {
            conn: schema::open(Some(db_path), JOBSTORE_MMAP_BYTES)?,
        })
    }

    pub fn open_in_memory() -> Result<Self, JobStoreError> {
        Ok(Self {
            conn: schema::open(None, JOBSTORE_MMAP_BYTES)?,
        })
    }

    /// Finished (completed or canceled) jobs cannot be resumed, so saving one deletes it.
    pub fn save_job(&self, job: &PersistedJob) -> Result<(), JobStoreError> {
        if matches!(job.state, JobState::Completed | JobState::Canceled) {
            self.conn.execute(
                "DELETE FROM persisted_jobs WHERE id = ?1",
                [job.id.0 as i64],
            )?;
            debug!(job_id = job.id.0, "jobstore.delete_finished_job");
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;

        tx.execute(
            "INSERT INTO persisted_jobs (id, state, created_at, updated_at, description)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                state = excluded.state,
                updated_at = excluded.updated_at,
                description = excluded.description",
            rusqlite::params![
                job.id.0 as i64,
                job_state_to_str(&job.state),
                job.created_at,
                job.updated_at,
                job.description,
            ],
        )?;

        for task in &job.tasks {
            tx.execute(
                "INSERT INTO persisted_tasks (id, job_id, kind, state, src_backend, src_stable_id, src_display_path, dst_backend, dst_stable_id, dst_display_path, bytes_total, bytes_completed, error_message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT (id) DO UPDATE SET
                    state = excluded.state,
                    bytes_completed = excluded.bytes_completed,
                    error_message = excluded.error_message",
                rusqlite::params![
                    task.id.0 as i64,
                    job.id.0 as i64,
                    task_kind_to_str(&task.kind),
                    task_state_to_str(&task.state),
                    task.src_backend,
                    task.src_stable_id,
                    task.src_display_path,
                    task.dst_backend,
                    task.dst_stable_id,
                    task.dst_display_path,
                    task.bytes_total as i64,
                    task.bytes_completed as i64,
                    task.error_message,
                ],
            )?;
        }

        tx.commit()?;
        debug!(
            job_id = job.id.0,
            task_count = job.tasks.len(),
            state = ?job.state,
            "jobstore.save_job"
        );
        Ok(())
    }

    pub fn load_job(&self, job_id: &JobId) -> Result<Option<PersistedJob>, JobStoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT state, created_at, updated_at, description
             FROM persisted_jobs WHERE id = ?1",
        )?;

        let job_result = stmt.query_row(rusqlite::params![job_id.0 as i64], |row| {
            let state_str: String = row.get(0)?;
            Ok(PersistedJob {
                id: job_id.clone(),
                state: str_to_job_state(&state_str),
                created_at: row.get(1)?,
                updated_at: row.get(2)?,
                description: row.get(3)?,
                tasks: Vec::new(),
            })
        });

        let mut job = match job_result {
            Ok(j) => j,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(JobStoreError::Database(e)),
        };

        job.tasks = self.load_tasks(job_id)?;
        Ok(Some(job))
    }

    pub fn load_all_incomplete_jobs(&self) -> Result<Vec<PersistedJob>, JobStoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, state, created_at, updated_at, description FROM persisted_jobs")?;

        let job_rows = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let state_str: String = row.get(1)?;
            Ok(PersistedJob {
                id: JobId(id as u64),
                state: str_to_job_state(&state_str),
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
                description: row.get(4)?,
                tasks: Vec::new(),
            })
        })?;

        let mut jobs = Vec::new();
        for row in job_rows {
            let mut job = row?;
            job.tasks = self.load_tasks(&job.id)?;
            jobs.push(job);
        }
        Ok(jobs)
    }

    pub fn update_task_progress(
        &self,
        task_id: &TaskId,
        bytes_completed: u64,
    ) -> Result<(), JobStoreError> {
        self.conn.execute(
            "UPDATE persisted_tasks SET bytes_completed = ?1 WHERE id = ?2",
            rusqlite::params![bytes_completed as i64, task_id.0 as i64],
        )?;
        debug!(
            task_id = task_id.0,
            bytes_completed, "jobstore.update_task_progress"
        );
        Ok(())
    }

    pub fn update_task_state(
        &self,
        task_id: &TaskId,
        state: &TaskState,
        error_message: Option<&str>,
    ) -> Result<(), JobStoreError> {
        self.conn.execute(
            "UPDATE persisted_tasks SET state = ?1, error_message = ?2 WHERE id = ?3",
            rusqlite::params![task_state_to_str(state), error_message, task_id.0 as i64],
        )?;
        debug!(
            task_id = task_id.0,
            state = ?state,
            has_error = error_message.is_some(),
            "jobstore.update_task_state"
        );
        Ok(())
    }

    pub fn update_job_state(
        &self,
        job_id: &JobId,
        state: &JobState,
        updated_at: &str,
    ) -> Result<(), JobStoreError> {
        self.conn.execute(
            "UPDATE persisted_jobs SET state = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![job_state_to_str(state), updated_at, job_id.0 as i64],
        )?;
        debug!(
            job_id = job_id.0,
            state = ?state,
            "jobstore.update_job_state"
        );
        Ok(())
    }

    pub fn upsert_chunk_state(&self, chunk: &PersistedChunkStateRow) -> Result<(), JobStoreError> {
        self.conn.execute(
            "INSERT INTO persisted_task_chunks (job_id, entry_index, offset, chunk_len, state, etag, checksum)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (job_id, entry_index, offset) DO UPDATE SET
                chunk_len = excluded.chunk_len,
                state = excluded.state,
                etag = excluded.etag,
                checksum = excluded.checksum",
            rusqlite::params![
                chunk.job_id.0 as i64,
                chunk.entry_index as i64,
                chunk.offset as i64,
                chunk.len as i64,
                chunk_state_to_str(&chunk.state),
                chunk.etag,
                chunk.checksum,
            ],
        )?;
        debug!(
            job_id = chunk.job_id.0,
            entry_index = chunk.entry_index,
            offset = chunk.offset,
            len = chunk.len,
            state = ?chunk.state,
            "jobstore.upsert_chunk_state"
        );
        Ok(())
    }

    pub fn load_chunk_states(
        &self,
        job_id: &JobId,
    ) -> Result<Vec<PersistedChunkStateRow>, JobStoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT entry_index, offset, chunk_len, state, etag, checksum
             FROM persisted_task_chunks
             WHERE job_id = ?1
             ORDER BY entry_index ASC, offset ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![job_id.0 as i64], |row| {
            let entry_index: i64 = row.get(0)?;
            let offset: i64 = row.get(1)?;
            let len: i64 = row.get(2)?;
            let state: String = row.get(3)?;
            Ok(PersistedChunkStateRow {
                job_id: job_id.clone(),
                entry_index: entry_index as u64,
                offset: offset as u64,
                len: len as u64,
                state: str_to_chunk_state(&state),
                etag: row.get(4)?,
                checksum: row.get(5)?,
            })
        })?;

        let mut chunks = Vec::new();
        for row in rows {
            chunks.push(row?);
        }
        Ok(chunks)
    }

    pub fn save_multipart_upload(
        &self,
        upload: &PersistedMultipartUpload,
    ) -> Result<(), JobStoreError> {
        self.conn.execute(
            "INSERT INTO persisted_multipart_uploads
                (job_id, entry_index, destination_backend, destination_path, source_identity, upload_id, total_len, part_size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (job_id, entry_index) DO UPDATE SET
                destination_backend = excluded.destination_backend,
                destination_path = excluded.destination_path,
                source_identity = excluded.source_identity,
                upload_id = excluded.upload_id,
                total_len = excluded.total_len,
                part_size = excluded.part_size",
            rusqlite::params![
                upload.job_id.0 as i64,
                upload.entry_index as i64,
                upload.destination_backend,
                upload.destination_path,
                upload.source_identity,
                upload.upload_id,
                upload.total_len as i64,
                upload.part_size as i64,
            ],
        )?;
        Ok(())
    }

    pub fn load_multipart_upload(
        &self,
        job_id: &JobId,
        entry_index: u64,
    ) -> Result<Option<PersistedMultipartUpload>, JobStoreError> {
        let result = self.conn.query_row(
            "SELECT destination_backend, destination_path, source_identity, upload_id, total_len, part_size
             FROM persisted_multipart_uploads WHERE job_id = ?1 AND entry_index = ?2",
            rusqlite::params![job_id.0 as i64, entry_index as i64],
            |row| {
                Ok(PersistedMultipartUpload {
                    job_id: job_id.clone(),
                    entry_index,
                    destination_backend: row.get(0)?,
                    destination_path: row.get(1)?,
                    source_identity: row.get(2)?,
                    upload_id: row.get(3)?,
                    total_len: row.get::<_, i64>(4)? as u64,
                    part_size: row.get::<_, i64>(5)? as u64,
                })
            },
        );
        match result {
            Ok(upload) => Ok(Some(upload)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn remove_multipart_upload(
        &self,
        job_id: &JobId,
        entry_index: u64,
    ) -> Result<(), JobStoreError> {
        self.conn.execute(
            "DELETE FROM persisted_multipart_uploads WHERE job_id = ?1 AND entry_index = ?2",
            rusqlite::params![job_id.0 as i64, entry_index as i64],
        )?;
        Ok(())
    }

    fn load_tasks(&self, job_id: &JobId) -> Result<Vec<PersistedTask>, JobStoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, state, src_backend, src_stable_id, src_display_path, dst_backend, dst_stable_id, dst_display_path, bytes_total, bytes_completed, error_message
             FROM persisted_tasks WHERE job_id = ?1",
        )?;

        let rows = stmt.query_map(rusqlite::params![job_id.0 as i64], |row| {
            let id: i64 = row.get(0)?;
            let kind_str: String = row.get(1)?;
            let state_str: String = row.get(2)?;
            let bytes_total: i64 = row.get(9)?;
            let bytes_completed: i64 = row.get(10)?;

            Ok(PersistedTask {
                id: TaskId(id as u64),
                job_id: job_id.clone(),
                kind: str_to_task_kind(&kind_str),
                state: str_to_task_state(&state_str),
                src_backend: row.get(3)?,
                src_stable_id: row.get(4)?,
                src_display_path: row.get(5)?,
                dst_backend: row.get(6)?,
                dst_stable_id: row.get(7)?,
                dst_display_path: row.get(8)?,
                bytes_total: bytes_total as u64,
                bytes_completed: bytes_completed as u64,
                error_message: row.get(11)?,
            })
        })?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }
}

fn job_state_to_str(state: &JobState) -> &'static str {
    match state {
        JobState::Queued => "Queued",
        JobState::Running => "Running",
        JobState::Paused => "Paused",
        JobState::Verifying => "Verifying",
        JobState::WaitingRemote => "WaitingRemote",
        JobState::UnknownRemoteState => "UnknownRemoteState",
        JobState::Completed => "Completed",
        JobState::Failed => "Failed",
        JobState::Canceled => "Canceled",
    }
}

fn str_to_job_state(s: &str) -> JobState {
    match s {
        "Queued" => JobState::Queued,
        "Running" => JobState::Running,
        "Paused" => JobState::Paused,
        "Verifying" => JobState::Verifying,
        "WaitingRemote" => JobState::WaitingRemote,
        "Completed" => JobState::Completed,
        "Failed" => JobState::Failed,
        "Canceled" => JobState::Canceled,
        // Unknown persisted states become UnknownRemoteState so the caller re-checks them.
        _ => JobState::UnknownRemoteState,
    }
}

fn task_kind_to_str(kind: &TaskKind) -> &'static str {
    match kind {
        TaskKind::Copy => "Copy",
        TaskKind::Move => "Move",
        TaskKind::Mkdir => "Mkdir",
        TaskKind::VerifyLight => "VerifyLight",
        TaskKind::VerifyStrong => "VerifyStrong",
        TaskKind::DeleteSource => "DeleteSource",
    }
}

fn str_to_task_kind(s: &str) -> TaskKind {
    match s {
        "Move" => TaskKind::Move,
        "Mkdir" => TaskKind::Mkdir,
        "VerifyLight" => TaskKind::VerifyLight,
        "VerifyStrong" => TaskKind::VerifyStrong,
        "DeleteSource" => TaskKind::DeleteSource,
        _ => TaskKind::Copy,
    }
}

fn task_state_to_str(state: &TaskState) -> &'static str {
    match state {
        TaskState::Pending => "Pending",
        TaskState::Running => "Running",
        TaskState::Completed => "Completed",
        TaskState::Failed => "Failed",
        TaskState::Skipped => "Skipped",
    }
}

fn str_to_task_state(s: &str) -> TaskState {
    match s {
        "Pending" => TaskState::Pending,
        "Running" => TaskState::Running,
        "Completed" => TaskState::Completed,
        "Skipped" => TaskState::Skipped,
        _ => TaskState::Failed,
    }
}

fn chunk_state_to_str(state: &PersistedChunkState) -> &'static str {
    match state {
        PersistedChunkState::Pending => "Pending",
        PersistedChunkState::Running => "Running",
        PersistedChunkState::Completed => "Completed",
        PersistedChunkState::Failed => "Failed",
    }
}

fn str_to_chunk_state(s: &str) -> PersistedChunkState {
    match s {
        "Pending" => PersistedChunkState::Pending,
        "Running" => PersistedChunkState::Running,
        "Completed" => PersistedChunkState::Completed,
        _ => PersistedChunkState::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_job() -> PersistedJob {
        PersistedJob {
            id: JobId(1),
            state: JobState::Running,
            created_at: "2026-03-20T10:00:00Z".to_string(),
            updated_at: "2026-03-20T10:01:00Z".to_string(),
            description: "Copy 10 files from local to NAS".to_string(),
            tasks: vec![
                PersistedTask {
                    id: TaskId(1),
                    job_id: JobId(1),
                    kind: TaskKind::Copy,
                    state: TaskState::Completed,
                    src_backend: Some("local-1".to_string()),
                    src_stable_id: Some("inode-100".to_string()),
                    src_display_path: Some("/src/a.jpg".to_string()),
                    dst_backend: Some("nas-1".to_string()),
                    dst_stable_id: Some("inode-200".to_string()),
                    dst_display_path: Some("/dst/a.jpg".to_string()),
                    bytes_total: 1024,
                    bytes_completed: 1024,
                    error_message: None,
                },
                PersistedTask {
                    id: TaskId(2),
                    job_id: JobId(1),
                    kind: TaskKind::VerifyLight,
                    state: TaskState::Pending,
                    src_backend: Some("local-1".to_string()),
                    src_stable_id: Some("inode-100".to_string()),
                    src_display_path: Some("/src/a.jpg".to_string()),
                    dst_backend: Some("nas-1".to_string()),
                    dst_stable_id: Some("inode-200".to_string()),
                    dst_display_path: Some("/dst/a.jpg".to_string()),
                    bytes_total: 0,
                    bytes_completed: 0,
                    error_message: None,
                },
            ],
        }
    }

    #[test]
    fn test_save_and_load_job() {
        let repo = JobStoreRepository::open_in_memory().unwrap();
        let job = make_test_job();

        repo.save_job(&job).unwrap();
        let loaded = repo.load_job(&JobId(1)).unwrap().unwrap();
        assert_eq!(loaded.description, "Copy 10 files from local to NAS");
        assert_eq!(loaded.tasks.len(), 2);
        assert_eq!(loaded.tasks[0].state, TaskState::Completed);
        assert_eq!(loaded.tasks[1].state, TaskState::Pending);
        assert_eq!(
            loaded.tasks[0].src_display_path.as_deref(),
            Some("/src/a.jpg")
        );
        assert_eq!(
            loaded.tasks[0].dst_display_path.as_deref(),
            Some("/dst/a.jpg")
        );
    }

    #[test]
    fn test_load_incomplete_jobs() {
        let repo = JobStoreRepository::open_in_memory().unwrap();
        let job = make_test_job();
        repo.save_job(&job).unwrap();

        let mut completed_job = make_test_job();
        completed_job.id = JobId(2);
        completed_job.state = JobState::Completed;
        completed_job.tasks[0].id = TaskId(3);
        completed_job.tasks[0].job_id = JobId(2);
        completed_job.tasks[1].id = TaskId(4);
        completed_job.tasks[1].job_id = JobId(2);
        repo.save_job(&completed_job).unwrap();

        let incomplete = repo.load_all_incomplete_jobs().unwrap();
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].id, JobId(1));
    }

    #[test]
    fn test_update_task_progress() {
        let repo = JobStoreRepository::open_in_memory().unwrap();
        let job = make_test_job();
        repo.save_job(&job).unwrap();

        repo.update_task_progress(&TaskId(1), 512).unwrap();
        let loaded = repo.load_job(&JobId(1)).unwrap().unwrap();
        assert_eq!(loaded.tasks[0].bytes_completed, 512);
    }

    #[test]
    fn test_update_task_state() {
        let repo = JobStoreRepository::open_in_memory().unwrap();
        let job = make_test_job();
        repo.save_job(&job).unwrap();

        repo.update_task_state(&TaskId(2), &TaskState::Failed, Some("disk full"))
            .unwrap();
        let loaded = repo.load_job(&JobId(1)).unwrap().unwrap();
        assert_eq!(loaded.tasks[1].state, TaskState::Failed);
        assert_eq!(loaded.tasks[1].error_message.as_deref(), Some("disk full"));
    }

    #[test]
    fn test_chunk_state_round_trip_and_update() {
        let repo = JobStoreRepository::open_in_memory().unwrap();
        let job = make_test_job();
        repo.save_job(&job).unwrap();

        let mut chunk = PersistedChunkStateRow {
            job_id: JobId(1),
            entry_index: 3,
            offset: 8 * 1024 * 1024,
            len: 8 * 1024 * 1024,
            state: PersistedChunkState::Running,
            etag: Some("etag-a".to_string()),
            checksum: None,
        };
        repo.upsert_chunk_state(&chunk).unwrap();

        chunk.state = PersistedChunkState::Completed;
        chunk.checksum = Some("sha256:abc".to_string());
        repo.upsert_chunk_state(&chunk).unwrap();

        let chunks = repo.load_chunk_states(&JobId(1)).unwrap();
        assert_eq!(chunks, vec![chunk]);
    }

    #[test]
    fn test_multipart_upload_round_trip_and_cleanup() {
        let repo = JobStoreRepository::open_in_memory().unwrap();
        repo.save_job(&make_test_job()).unwrap();
        let upload = PersistedMultipartUpload {
            job_id: JobId(1),
            entry_index: 3,
            destination_backend: "s3:archive".into(),
            destination_path: "backups/image.bin".into(),
            source_identity: Some("etag:source-etag".into()),
            upload_id: "upload-17".into(),
            total_len: 33,
            part_size: 16,
        };
        repo.save_multipart_upload(&upload).unwrap();
        assert_eq!(
            repo.load_multipart_upload(&JobId(1), 3).unwrap(),
            Some(upload)
        );
        repo.remove_multipart_upload(&JobId(1), 3).unwrap();
        assert!(repo.load_multipart_upload(&JobId(1), 3).unwrap().is_none());
    }

    #[test]
    fn saving_finished_job_removes_it() {
        let repo = JobStoreRepository::open_in_memory().unwrap();
        let mut job = make_test_job();
        repo.save_job(&job).unwrap();

        job.state = JobState::Completed;
        repo.save_job(&job).unwrap();
        assert!(repo.load_job(&JobId(1)).unwrap().is_none());
    }
}
