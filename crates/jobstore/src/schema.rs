use rusqlite::Connection;

const SCHEMA_VERSION: i64 = 1;

/// A database with a different schema version holds jobs that cannot be resumed; it is rebuilt.
pub fn initialize_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        drop_all_tables(conn)?;
    }
    conn.execute_batch(
        "
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

        CREATE TABLE IF NOT EXISTS persisted_multipart_uploads (
            job_id          INTEGER NOT NULL,
            entry_index     INTEGER NOT NULL,
            destination_backend TEXT NOT NULL,
            destination_path TEXT NOT NULL,
            source_identity TEXT,
            upload_id       TEXT NOT NULL,
            total_len       INTEGER NOT NULL,
            part_size       INTEGER NOT NULL,
            PRIMARY KEY (job_id, entry_index),
            FOREIGN KEY (job_id) REFERENCES persisted_jobs (id) ON DELETE CASCADE
        );
        ",
    )?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
}

/// Foreign keys are disabled during the drop so table order does not matter.
fn drop_all_tables(conn: &Connection) -> Result<(), rusqlite::Error> {
    let tables = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    for table in tables {
        conn.execute_batch(&format!("DROP TABLE \"{}\";", table.replace('"', "\"\"")))?;
    }
    conn.execute_batch("PRAGMA foreign_keys=ON;")
}
