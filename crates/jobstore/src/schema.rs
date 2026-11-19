use rusqlite::Connection;
use std::path::Path;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS persisted_jobs (
    id          INTEGER PRIMARY KEY,
    state       TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    description TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS persisted_tasks (
    id              INTEGER PRIMARY KEY,
    job_id          INTEGER NOT NULL,
    kind            TEXT NOT NULL,
    state           TEXT NOT NULL,
    src_backend     TEXT,
    src_stable_id   TEXT,
    src_display_path TEXT,
    dst_backend     TEXT,
    dst_stable_id   TEXT,
    dst_display_path TEXT,
    installed_path  TEXT,
    bytes_total     INTEGER NOT NULL DEFAULT 0,
    bytes_completed INTEGER NOT NULL DEFAULT 0,
    error_message   TEXT,
    FOREIGN KEY (job_id) REFERENCES persisted_jobs (id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_persisted_tasks_job_id
    ON persisted_tasks (job_id);

CREATE TABLE IF NOT EXISTS persisted_task_chunks (
    job_id      INTEGER NOT NULL,
    entry_index INTEGER NOT NULL,
    offset      INTEGER NOT NULL,
    chunk_len   INTEGER NOT NULL,
    state       TEXT NOT NULL,
    etag        TEXT,
    checksum    TEXT,
    PRIMARY KEY (job_id, entry_index, offset),
    FOREIGN KEY (job_id) REFERENCES persisted_jobs (id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_persisted_task_chunks_job_id
    ON persisted_task_chunks (job_id);

CREATE TABLE IF NOT EXISTS persisted_range_sessions (
    job_id INTEGER NOT NULL,
    entry_index INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    binding TEXT NOT NULL,
    staging_path TEXT NOT NULL,
    total_len INTEGER NOT NULL,
    file_identity TEXT,
    checksum_barrier_generation INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (job_id, entry_index),
    FOREIGN KEY (job_id) REFERENCES persisted_jobs (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS persisted_multipart_uploads (
    job_id          INTEGER NOT NULL,
    entry_index     INTEGER NOT NULL,
    destination_backend TEXT NOT NULL,
    destination_path TEXT NOT NULL,
    source_identity TEXT,
    upload_id       TEXT NOT NULL,
    total_len       INTEGER NOT NULL,
    part_size       INTEGER NOT NULL,
    cleanup_pending INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (job_id, entry_index),
    FOREIGN KEY (job_id) REFERENCES persisted_jobs (id) ON DELETE CASCADE
);
";

/// A database with a different schema version holds jobs that cannot be resumed; it is rebuilt.
pub fn open(path: Option<&Path>, mmap_bytes: u64) -> rusqlite::Result<Connection> {
    let conn = iris_core_domain::sqlite::open(path, mmap_bytes, SCHEMA_VERSION, SCHEMA)?;
    // Additive migrations: rebuilding the store would orphan resumable uploads.
    // Legacy completed tasks keep a null installed path: no durable proof exists
    // of which sibling was installed.
    let has_installed_path: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('persisted_tasks')
         WHERE name = 'installed_path')",
        [],
        |row| row.get(0),
    )?;
    if !has_installed_path {
        conn.execute_batch("ALTER TABLE persisted_tasks ADD COLUMN installed_path TEXT;")?;
    }
    let has_cleanup_pending: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('persisted_multipart_uploads')
         WHERE name = 'cleanup_pending')",
        [],
        |row| row.get(0),
    )?;
    if !has_cleanup_pending {
        conn.execute_batch(
            "ALTER TABLE persisted_multipart_uploads
             ADD COLUMN cleanup_pending INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    let has_file_identity: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('persisted_range_sessions')
         WHERE name = 'file_identity')",
        [],
        |row| row.get(0),
    )?;
    if !has_file_identity {
        conn.execute_batch("ALTER TABLE persisted_range_sessions ADD COLUMN file_identity TEXT;")?;
    }
    let has_checksum_barrier_generation: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('persisted_range_sessions')
         WHERE name = 'checksum_barrier_generation')",
        [],
        |row| row.get(0),
    )?;
    if !has_checksum_barrier_generation {
        conn.execute_batch(
            "ALTER TABLE persisted_range_sessions
             ADD COLUMN checksum_barrier_generation INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(conn)
}
