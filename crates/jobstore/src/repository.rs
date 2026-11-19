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
    /// The path actually installed on completion; null for legacy tasks whose
    /// installation path was never recorded.
    pub installed_path: Option<String>,
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
    /// A rejected completion may only be aborted, never recovered as a successful copy.
    pub cleanup_pending: bool,
}

/// The row is durable before staging is created. Its binding covers the exact
/// job, entry, source version, destination and range layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedRangeSession {
    pub job_id: JobId,
    pub entry_index: u64,
    pub session_id: String,
    pub binding: String,
    pub staging_path: String,
    pub file_identity: Option<String>,
    pub total_len: u64,
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
            let tx = rusqlite::Transaction::new_unchecked(
                &self.conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            self.cleanup_range_sessions(&job.id)?;
            self.conn.execute(
                "DELETE FROM persisted_jobs WHERE id = ?1",
                [job.id.0 as i64],
            )?;
            tx.commit()?;
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
                "INSERT INTO persisted_tasks (id, job_id, kind, state, src_backend, src_stable_id, src_display_path, dst_backend, dst_stable_id, dst_display_path, installed_path, bytes_total, bytes_completed, error_message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT (id) DO UPDATE SET
                    state = excluded.state,
                    bytes_completed = excluded.bytes_completed,
                    error_message = excluded.error_message,
                    installed_path = COALESCE(excluded.installed_path, persisted_tasks.installed_path)",
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
                    task.installed_path,
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
        self.update_task_state_with_installed_path(task_id, state, error_message, None)
    }

    /// Records the installed path only on completion. Other states must not
    /// overwrite durable evidence from an earlier attempt.
    pub fn update_task_state_with_installed_path(
        &self,
        task_id: &TaskId,
        state: &TaskState,
        error_message: Option<&str>,
        installed_path: Option<&str>,
    ) -> Result<(), JobStoreError> {
        self.conn.execute(
            "UPDATE persisted_tasks SET state = ?1, error_message = ?2,
             installed_path = CASE WHEN ?1 = 'Completed' THEN ?4 ELSE installed_path END
             WHERE id = ?3",
            rusqlite::params![
                task_state_to_str(state),
                error_message,
                task_id.0 as i64,
                installed_path
            ],
        )?;
        debug!(
            task_id = task_id.0,
            state = ?state,
            has_error = error_message.is_some(),
            has_installed_path = installed_path.is_some(),
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
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if matches!(state, JobState::Completed | JobState::Canceled) {
            self.cleanup_range_sessions(job_id)?;
        }
        self.conn.execute(
            "UPDATE persisted_jobs SET state = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![job_state_to_str(state), updated_at, job_id.0 as i64],
        )?;
        tx.commit()?;
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

    pub fn save_range_session(&self, session: &PersistedRangeSession) -> Result<(), JobStoreError> {
        if session.file_identity.is_some() {
            return Err(JobStoreError::Io(std::io::Error::other(
                "new range session already has file identity",
            )));
        }
        // Insert only: a competing attempt must not replace another owner's journal.
        let inserted = self.conn.execute(
            "INSERT INTO persisted_range_sessions (job_id, entry_index, session_id, binding, staging_path, total_len)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6 FROM persisted_jobs WHERE id = ?1 AND state NOT IN ('Completed', 'Canceled')",
            rusqlite::params![session.job_id.0 as i64, session.entry_index as i64, session.session_id, session.binding, session.staging_path, session.total_len as i64],
        )?;
        if inserted != 1 {
            return Err(JobStoreError::Io(std::io::Error::other(
                "range session requires a resumable job",
            )));
        }
        // The row is committed before filesystem creation. Reacquire a write
        // transaction and recheck it so deletion cannot race creation, while a
        // crash can never leave unjournaled staging behind.
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if self
            .load_range_session(&session.job_id, session.entry_index)?
            .as_ref()
            != Some(session)
        {
            return Err(JobStoreError::Io(std::io::Error::other(
                "range session was removed before creation",
            )));
        }
        if let Err(error) = iris_platform_integration::staging::OwnedStage::create(
            &iris_platform_integration::staging::decode_path(&session.staging_path)?,
            &session.binding,
            session.total_len,
        ) {
            // Creation cleans its own partial directory. If cleanup cannot finish,
            // retain the durable intent so a later attempt can safely recover it.
            if error.kind() != std::io::ErrorKind::AlreadyExists
                && self
                    .cleanup_range_session(&session.job_id, session.entry_index)
                    .is_ok()
            {
                tx.commit()?;
            }
            return Err(error.into());
        }
        tx.commit()?;
        Ok(())
    }

    /// Bind only the exact observed session; never replace an existing proof.
    pub fn bind_range_file_identity(
        &self,
        session: &PersistedRangeSession,
        identity: &str,
    ) -> Result<(), JobStoreError> {
        // The file may be installed immediately after this acknowledgement.
        // Unlike ordinary progress rows, this proof must survive a power loss.
        let synchronous: i64 = self
            .conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))?;
        self.conn.pragma_update(None, "synchronous", "FULL")?;
        let result = self.conn.execute(
            "UPDATE persisted_range_sessions SET file_identity = ?1
             WHERE job_id = ?2 AND entry_index = ?3 AND session_id = ?4
             AND binding = ?5 AND staging_path = ?6 AND total_len = ?7
             AND file_identity IS ?8 AND (file_identity IS NULL OR file_identity = ?1)",
            rusqlite::params![
                identity,
                session.job_id.0 as i64,
                session.entry_index as i64,
                session.session_id,
                session.binding,
                session.staging_path,
                session.total_len as i64,
                session.file_identity
            ],
        );
        self.conn.pragma_update(None, "synchronous", synchronous)?;
        if result? != 1 {
            return Err(JobStoreError::Io(std::io::Error::other(
                "range file identity changed",
            )));
        }
        Ok(())
    }

    /// Validates every completed checksum and advances a durable session marker
    /// in one FULL-synchronous transaction before the stage may be installed.
    /// The marker write forces a WAL commit that synchronizes prior NORMAL writes.
    pub fn persist_range_chunk_checksums(
        &self,
        session: &PersistedRangeSession,
        ranges: &[(u64, u64)],
    ) -> Result<(), JobStoreError> {
        use rusqlite::OptionalExtension;

        let invalid = || {
            JobStoreError::Io(std::io::Error::other(
                "range checksum journal does not match installation plan",
            ))
        };
        let job_id = i64::try_from(session.job_id.0).map_err(|_| invalid())?;
        let entry_index = i64::try_from(session.entry_index).map_err(|_| invalid())?;
        if ranges.is_empty() || i64::try_from(session.total_len).is_err() {
            return Err(invalid());
        }
        let mut next_offset = 0u64;
        for (offset, len) in ranges {
            if *offset != next_offset
                || *len == 0
                || i64::try_from(*offset).is_err()
                || i64::try_from(*len).is_err()
            {
                return Err(invalid());
            }
            next_offset = offset.checked_add(*len).ok_or_else(invalid)?;
        }
        if next_offset != session.total_len {
            return Err(invalid());
        }

        let stage_etag = format!("stage:{}", session.session_id);
        let synchronous: i64 = self
            .conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))?;
        self.conn.pragma_update(None, "synchronous", "FULL")?;
        let result = (|| {
            let tx = rusqlite::Transaction::new_unchecked(
                &self.conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            if self
                .load_range_session(&session.job_id, session.entry_index)?
                .as_ref()
                != Some(session)
            {
                return Err(JobStoreError::Io(std::io::Error::other(
                    "range session changed before checksum durability barrier",
                )));
            }
            let row_count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM persisted_task_chunks
                 WHERE job_id = ?1 AND entry_index = ?2 AND etag = ?3",
                rusqlite::params![job_id, entry_index, stage_etag],
                |row| row.get(0),
            )?;
            if usize::try_from(row_count).ok() != Some(ranges.len()) {
                return Err(invalid());
            }

            for (offset, len) in ranges {
                let row = tx
                    .query_row(
                        "SELECT chunk_len, state, etag, checksum
                         FROM persisted_task_chunks
                         WHERE job_id = ?1 AND entry_index = ?2 AND offset = ?3",
                        rusqlite::params![job_id, entry_index, *offset as i64],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, Option<String>>(2)?,
                                row.get::<_, Option<String>>(3)?,
                            ))
                        },
                    )
                    .optional()?
                    .ok_or_else(invalid)?;
                let (stored_len, state, etag, checksum) = row;
                let valid_checksum = checksum.as_deref().is_some_and(|value| {
                    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                });
                if stored_len != *len as i64
                    || state != "Completed"
                    || etag.as_deref() != Some(stage_etag.as_str())
                    || !valid_checksum
                {
                    return Err(invalid());
                }
            }
            let marked = tx.execute(
                "UPDATE persisted_range_sessions
                 SET checksum_barrier_generation = checksum_barrier_generation + 1
                 WHERE job_id = ?1 AND entry_index = ?2 AND session_id = ?3
                   AND binding = ?4 AND staging_path = ?5 AND total_len = ?6
                   AND file_identity IS ?7
                   AND typeof(checksum_barrier_generation) = 'integer'
                   AND checksum_barrier_generation >= 0
                   AND checksum_barrier_generation < ?8",
                rusqlite::params![
                    job_id,
                    entry_index,
                    session.session_id,
                    session.binding,
                    session.staging_path,
                    session.total_len as i64,
                    session.file_identity,
                    i64::MAX
                ],
            )?;
            if marked != 1 {
                return Err(invalid());
            }
            tx.commit()?;
            Ok(())
        })();
        self.conn
            .pragma_update(None, "synchronous", synchronous)
            .map_err(JobStoreError::from)?;
        result
    }

    pub fn load_range_session(
        &self,
        job_id: &JobId,
        entry_index: u64,
    ) -> Result<Option<PersistedRangeSession>, JobStoreError> {
        use rusqlite::OptionalExtension;
        Ok(self.conn.query_row(
            "SELECT session_id, binding, staging_path, total_len, file_identity FROM persisted_range_sessions WHERE job_id = ?1 AND entry_index = ?2",
            rusqlite::params![job_id.0 as i64, entry_index as i64],
            |row| Ok(PersistedRangeSession { job_id: job_id.clone(), entry_index, session_id: row.get(0)?, binding: row.get(1)?, staging_path: row.get(2)?, total_len: row.get::<_, i64>(3)? as u64, file_identity: row.get(4)? }),
        ).optional()?)
    }

    pub fn remove_range_session(
        &self,
        job_id: &JobId,
        entry_index: u64,
    ) -> Result<(), JobStoreError> {
        self.remove_range_session_checked(job_id, entry_index, None)
    }

    /// A transfer may discard only the exact session it observed, not a successor
    /// admitted by another attempt between its read and this write transaction.
    pub fn discard_range_session(
        &self,
        session: &PersistedRangeSession,
    ) -> Result<(), JobStoreError> {
        self.remove_range_session_checked(&session.job_id, session.entry_index, Some(session))
    }

    fn remove_range_session_checked(
        &self,
        job_id: &JobId,
        entry_index: u64,
        expected: Option<&PersistedRangeSession>,
    ) -> Result<(), JobStoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if let Some(expected) = expected {
            if self.load_range_session(job_id, entry_index)?.as_ref() != Some(expected) {
                return Err(JobStoreError::Io(std::io::Error::other(
                    "range session changed before cleanup",
                )));
            }
        }
        self.cleanup_range_session(job_id, entry_index)?;
        tx.commit()?;
        Ok(())
    }

    /// Caller holds the write transaction, excluding new session admission until
    /// both filesystem cleanup and journal deletion have completed.
    fn cleanup_range_session(&self, job_id: &JobId, entry_index: u64) -> Result<(), JobStoreError> {
        if let Some(session) = self.load_range_session(job_id, entry_index)? {
            iris_platform_integration::staging::remove(
                &iris_platform_integration::staging::decode_path(&session.staging_path)?,
                &session.binding,
                session.total_len,
            )?;
        }
        for table in ["persisted_range_sessions", "persisted_task_chunks"] {
            self.conn.execute(
                &format!("DELETE FROM {table} WHERE job_id = ?1 AND entry_index = ?2"),
                rusqlite::params![job_id.0 as i64, entry_index as i64],
            )?;
        }
        Ok(())
    }

    fn cleanup_range_sessions(&self, job_id: &JobId) -> Result<(), JobStoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT entry_index FROM persisted_range_sessions WHERE job_id = ?1")?;
        let entries = stmt
            .query_map([job_id.0 as i64], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for entry in entries {
            self.cleanup_range_session(job_id, entry as u64)?;
        }
        Ok(())
    }

    pub fn save_multipart_upload(
        &self,
        upload: &PersistedMultipartUpload,
    ) -> Result<(), JobStoreError> {
        self.conn.execute(
            "INSERT INTO persisted_multipart_uploads
                (job_id, entry_index, destination_backend, destination_path, source_identity, upload_id, total_len, part_size, cleanup_pending)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (job_id, entry_index) DO UPDATE SET
                destination_backend = excluded.destination_backend,
                destination_path = excluded.destination_path,
                source_identity = excluded.source_identity,
                upload_id = excluded.upload_id,
                total_len = excluded.total_len,
                part_size = excluded.part_size,
                cleanup_pending = excluded.cleanup_pending",
            rusqlite::params![
                upload.job_id.0 as i64,
                upload.entry_index as i64,
                upload.destination_backend,
                upload.destination_path,
                upload.source_identity,
                upload.upload_id,
                upload.total_len as i64,
                upload.part_size as i64,
                upload.cleanup_pending,
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
            "SELECT destination_backend, destination_path, source_identity, upload_id, total_len, part_size, cleanup_pending
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
                    cleanup_pending: row.get(6)?,
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
            "SELECT id, kind, state, src_backend, src_stable_id, src_display_path, dst_backend, dst_stable_id, dst_display_path, bytes_total, bytes_completed, error_message, installed_path
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
                installed_path: row.get(12)?,
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
                    installed_path: None,
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
                    installed_path: None,
                    bytes_total: 0,
                    bytes_completed: 0,
                    error_message: None,
                },
            ],
        }
    }

    #[test]
    fn range_creation_failure_and_stale_cleanup_do_not_strand_or_replace_sessions() {
        use iris_platform_integration::staging::encode_path;
        let repo = JobStoreRepository::open_in_memory().unwrap();
        let job = make_test_job();
        repo.save_job(&job).unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("stage");
        let mut session = PersistedRangeSession {
            file_identity: None,
            job_id: job.id.clone(),
            entry_index: 0,
            session_id: "a".into(),
            binding: "bound".into(),
            staging_path: encode_path(&path),
            total_len: u64::MAX,
        };
        // A real partial create: intent and data are created, but this length
        // cannot be allocated by the local filesystem API.
        assert!(repo.save_range_session(&session).is_err());
        assert!(!path.exists());
        assert!(repo.load_range_session(&job.id, 0).unwrap().is_none());
        session.total_len = 4;
        repo.save_range_session(&session).unwrap();
        let mut stale = session.clone();
        stale.session_id = "b".into();
        assert!(repo.discard_range_session(&stale).is_err());
        assert!(path.join("data").exists());
        assert_eq!(
            repo.load_range_session(&job.id, 0).unwrap(),
            Some(session.clone())
        );
        repo.discard_range_session(&session).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn range_identity_migration_cas_and_interrupted_cleanup_preserve_proof() {
        use iris_platform_integration::staging::{encode_path, OwnedStage};
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("jobs.sqlite3");
        let stage_path = root.path().join("stage");
        let repo = JobStoreRepository::open(&db).unwrap();
        let job = make_test_job();
        repo.save_job(&job).unwrap();
        let mut session = PersistedRangeSession {
            job_id: job.id.clone(),
            entry_index: 0,
            session_id: "session".into(),
            binding: "job-entry-source-destination-layout".into(),
            staging_path: encode_path(&stage_path),
            total_len: 4,
            file_identity: None,
        };
        repo.save_range_session(&session).unwrap();
        // A v1 store predating the additive proof fields retains its jobs and
        // journal; normal stage validation still gates installation.
        repo.conn
            .execute_batch(
                "ALTER TABLE persisted_range_sessions DROP COLUMN checksum_barrier_generation;
                 ALTER TABLE persisted_range_sessions DROP COLUMN file_identity;",
            )
            .unwrap();
        drop(repo);
        let repo = JobStoreRepository::open(&db).unwrap();
        assert_eq!(
            repo.load_range_session(&job.id, 0).unwrap(),
            Some(session.clone())
        );
        let generation: i64 = repo
            .conn
            .query_row(
                "SELECT checksum_barrier_generation FROM persisted_range_sessions
                 WHERE job_id = ?1 AND entry_index = ?2",
                rusqlite::params![job.id.0 as i64, session.entry_index as i64],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generation, 0);
        assert_eq!(
            repo.load_job(&job.id).unwrap().unwrap().tasks.len(),
            job.tasks.len()
        );
        let stage = OwnedStage::reopen(&stage_path, &session.binding, 4).unwrap();
        let identity = stage.file_identity().unwrap();
        let mut stale = session.clone();
        stale.binding = "other".into();
        assert!(repo.bind_range_file_identity(&stale, &identity).is_err());
        repo.bind_range_file_identity(&session, &identity).unwrap();
        assert!(repo.bind_range_file_identity(&session, &identity).is_err());
        session.file_identity = Some(identity.clone());
        repo.bind_range_file_identity(&session, &identity).unwrap();
        assert!(repo.bind_range_file_identity(&session, "[0,0]").is_err());
        assert_eq!(
            repo.conn
                .pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        std::fs::rename(stage.path(), root.path().join("final")).unwrap();
        drop(stage);
        // Simulate the first deletion in filesystem cleanup, before SQLite's
        // session/chunk deletion commits. Installation proof stays in the journal.
        std::fs::remove_file(stage_path.join("identity")).unwrap();
        drop(repo);
        let repo = JobStoreRepository::open(&db).unwrap();
        assert_eq!(
            repo.load_range_session(&job.id, 0).unwrap(),
            Some(session.clone())
        );
        repo.discard_range_session(&session).unwrap();
        assert!(repo.load_range_session(&job.id, 0).unwrap().is_none());
        assert!(!stage_path.exists());
        assert_eq!(std::fs::read(root.path().join("final")).unwrap(), [0; 4]);
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
    fn installed_path_migrates_and_is_stored_on_completion() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("jobs.sqlite3");
        let repo = JobStoreRepository::open(&db).unwrap();
        let job = make_test_job();
        repo.save_job(&job).unwrap();

        // A v1 store predating the column keeps its jobs; legacy completions have
        // no evidence of which sibling was installed.
        repo.conn
            .execute_batch("ALTER TABLE persisted_tasks DROP COLUMN installed_path;")
            .unwrap();
        drop(repo);
        let repo = JobStoreRepository::open(&db).unwrap();
        let loaded = repo.load_job(&job.id).unwrap().unwrap();
        assert_eq!(loaded.tasks[0].state, TaskState::Completed);
        assert_eq!(loaded.tasks[0].installed_path, None);

        // A non-completion update never turns the requested path into evidence.
        repo.update_task_state_with_installed_path(
            &TaskId(1),
            &TaskState::Running,
            None,
            Some("/dst/a.jpg"),
        )
        .unwrap();
        assert_eq!(
            repo.load_job(&job.id).unwrap().unwrap().tasks[0].installed_path,
            None
        );

        // Completion persists the exact installed path, including a numbered
        // sibling and an ordinary path equal to the requested destination.
        repo.update_task_state_with_installed_path(
            &TaskId(1),
            &TaskState::Completed,
            None,
            Some("/dst/a (1).jpg"),
        )
        .unwrap();
        repo.update_task_state_with_installed_path(
            &TaskId(2),
            &TaskState::Completed,
            None,
            Some("/dst/b.jpg"),
        )
        .unwrap();
        let reopened = JobStoreRepository::open(&db).unwrap();
        let tasks = reopened.load_job(&job.id).unwrap().unwrap().tasks;
        assert_eq!(tasks[0].installed_path.as_deref(), Some("/dst/a (1).jpg"));
        assert_eq!(tasks[1].installed_path.as_deref(), Some("/dst/b.jpg"));

        // Re-saving a stale task snapshot must not discard recorded proof.
        let mut stale = loaded;
        stale.tasks[0].installed_path = None;
        reopened.save_job(&stale).unwrap();
        assert_eq!(
            reopened.load_job(&job.id).unwrap().unwrap().tasks[0]
                .installed_path
                .as_deref(),
            Some("/dst/a (1).jpg")
        );
    }

    #[test]
    fn range_checksum_barrier_commits_exact_session_with_full_sync() {
        use iris_platform_integration::staging::encode_path;

        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("jobs.sqlite3");
        let stage_path = root.path().join("stage");
        let repo = JobStoreRepository::open(&db).unwrap();
        let job = make_test_job();
        repo.save_job(&job).unwrap();
        let session = PersistedRangeSession {
            job_id: job.id.clone(),
            entry_index: 0,
            session_id: "durable-session".into(),
            binding: "source-and-destination-binding".into(),
            staging_path: encode_path(&stage_path),
            total_len: 8,
            file_identity: None,
        };
        repo.save_range_session(&session).unwrap();
        let ranges = [(0, 4), (4, 4)];
        let etag = format!("stage:{}", session.session_id);
        for (offset, len) in ranges {
            repo.upsert_chunk_state(&PersistedChunkStateRow {
                job_id: job.id.clone(),
                entry_index: session.entry_index,
                offset,
                len,
                state: PersistedChunkState::Completed,
                etag: Some(etag.clone()),
                checksum: Some("a".repeat(64)),
            })
            .unwrap();
        }
        assert_eq!(
            repo.conn
                .pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );

        repo.persist_range_chunk_checksums(&session, &ranges)
            .unwrap();
        repo.persist_range_chunk_checksums(&session, &ranges)
            .unwrap();
        let generation: i64 = repo
            .conn
            .query_row(
                "SELECT checksum_barrier_generation FROM persisted_range_sessions
                 WHERE job_id = ?1 AND entry_index = ?2",
                rusqlite::params![job.id.0 as i64, session.entry_index as i64],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generation, 2);
        assert_eq!(
            repo.conn
                .pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(repo);

        let reopened = JobStoreRepository::open(&db).unwrap();
        let generation: i64 = reopened
            .conn
            .query_row(
                "SELECT checksum_barrier_generation FROM persisted_range_sessions
                 WHERE job_id = ?1 AND entry_index = ?2",
                rusqlite::params![job.id.0 as i64, session.entry_index as i64],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generation, 2);
        let chunks = reopened.load_chunk_states(&job.id).unwrap();
        let expected_checksum = "a".repeat(64);
        assert_eq!(chunks.len(), ranges.len());
        assert!(chunks.iter().all(|chunk| {
            chunk.state == PersistedChunkState::Completed
                && chunk.etag.as_deref() == Some(etag.as_str())
                && chunk.checksum.as_deref() == Some(expected_checksum.as_str())
        }));
    }

    #[test]
    fn range_checksum_barrier_rejects_incomplete_rows_and_propagates_write_errors() {
        use iris_platform_integration::staging::encode_path;

        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("jobs.sqlite3");
        let stage_path = root.path().join("stage");
        let repo = JobStoreRepository::open(&db).unwrap();
        let job = make_test_job();
        repo.save_job(&job).unwrap();
        let session = PersistedRangeSession {
            job_id: job.id.clone(),
            entry_index: 0,
            session_id: "barrier-session".into(),
            binding: "source-and-destination-binding".into(),
            staging_path: encode_path(&stage_path),
            total_len: 8,
            file_identity: None,
        };
        repo.save_range_session(&session).unwrap();
        let ranges = [(0, 4), (4, 4)];
        let etag = format!("stage:{}", session.session_id);
        let mut second = PersistedChunkStateRow {
            job_id: job.id.clone(),
            entry_index: session.entry_index,
            offset: 4,
            len: 4,
            state: PersistedChunkState::Pending,
            etag: Some(etag.clone()),
            checksum: None,
        };
        repo.upsert_chunk_state(&PersistedChunkStateRow {
            job_id: job.id.clone(),
            entry_index: session.entry_index,
            offset: 0,
            len: 4,
            state: PersistedChunkState::Completed,
            etag: Some(etag.clone()),
            checksum: Some("b".repeat(64)),
        })
        .unwrap();
        repo.upsert_chunk_state(&second).unwrap();
        assert!(repo
            .persist_range_chunk_checksums(&session, &ranges)
            .is_err());
        repo.conn
            .execute(
                "DELETE FROM persisted_task_chunks WHERE job_id = ?1 AND entry_index = ?2 AND offset = 4",
                rusqlite::params![job.id.0 as i64, session.entry_index as i64],
            )
            .unwrap();
        assert!(repo
            .persist_range_chunk_checksums(&session, &ranges)
            .is_err());
        repo.upsert_chunk_state(&second).unwrap();
        assert!(repo
            .persist_range_chunk_checksums(&session, &[(0, 3), (4, 5)])
            .is_err());
        let mut stale_session = session.clone();
        stale_session.binding.push('x');
        assert!(repo
            .persist_range_chunk_checksums(&stale_session, &ranges)
            .is_err());
        assert_eq!(
            repo.conn
                .pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );

        second.state = PersistedChunkState::Completed;
        second.checksum = Some("bad".into());
        repo.upsert_chunk_state(&second).unwrap();
        assert!(repo
            .persist_range_chunk_checksums(&session, &ranges)
            .is_err());
        second.checksum = Some("c".repeat(64));
        repo.upsert_chunk_state(&second).unwrap();
        repo.conn
            .execute_batch(
                "CREATE TRIGGER fail_range_checksum_barrier
                 BEFORE UPDATE OF checksum_barrier_generation ON persisted_range_sessions
                 BEGIN SELECT RAISE(ABORT, 'injected checksum barrier failure'); END;",
            )
            .unwrap();
        assert!(repo
            .persist_range_chunk_checksums(&session, &ranges)
            .is_err());
        assert_eq!(
            repo.conn
                .pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        repo.conn
            .execute_batch("DROP TRIGGER fail_range_checksum_barrier;")
            .unwrap();
        repo.persist_range_chunk_checksums(&session, &ranges)
            .unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.sqlite3");
        let repo = JobStoreRepository::open(&path).unwrap();
        repo.save_job(&make_test_job()).unwrap();
        let mut upload = PersistedMultipartUpload {
            job_id: JobId(1),
            entry_index: 3,
            destination_backend: "s3:archive".into(),
            destination_path: "backups/image.bin".into(),
            source_identity: Some("etag:source-etag".into()),
            upload_id: "upload-17".into(),
            total_len: 33,
            part_size: 16,
            cleanup_pending: false,
        };
        repo.save_multipart_upload(&upload).unwrap();
        // Recreate the previous schema with a live upload. Opening must migrate
        // in place rather than rebuilding and orphaning the remote session.
        repo.conn
            .execute_batch("ALTER TABLE persisted_multipart_uploads DROP COLUMN cleanup_pending;")
            .unwrap();
        drop(repo);
        let repo = JobStoreRepository::open(&path).unwrap();
        assert_eq!(
            repo.load_multipart_upload(&JobId(1), 3).unwrap(),
            Some(upload.clone())
        );
        upload.cleanup_pending = true;
        repo.save_multipart_upload(&upload).unwrap();
        drop(repo);
        let repo = JobStoreRepository::open(&path).unwrap();
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
