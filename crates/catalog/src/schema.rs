use rusqlite::Connection;

const SCHEMA_VERSION: i64 = 1;

/// A database with a different schema version is a disposable cache; it is rebuilt.
pub fn initialize_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        drop_all_tables(conn)?;
    }
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS cached_objects (
            backend_id   TEXT NOT NULL,
            stable_id    TEXT NOT NULL,
            display_path TEXT NOT NULL,
            size         INTEGER,
            mtime_secs   INTEGER,
            mtime_nanos  INTEGER,
            content_type TEXT,
            etag         TEXT,
            cached_level INTEGER NOT NULL DEFAULT 0,
            last_accessed_secs INTEGER NOT NULL,
            PRIMARY KEY (backend_id, stable_id)
        );

        CREATE INDEX IF NOT EXISTS idx_cached_objects_last_accessed
            ON cached_objects (last_accessed_secs);

        CREATE TABLE IF NOT EXISTS thumbnail_entries (
            backend_id      TEXT NOT NULL,
            stable_id       TEXT NOT NULL,
            dimension       INTEGER NOT NULL,
            format          TEXT NOT NULL,
            cache_file_path TEXT NOT NULL,
            byte_size       INTEGER NOT NULL DEFAULT 0,
            generated_at_secs INTEGER NOT NULL,
            source_size     INTEGER,
            source_mtime_secs INTEGER,
            source_mtime_nanos INTEGER,
            source_etag     TEXT,
            PRIMARY KEY (backend_id, stable_id, dimension),
            FOREIGN KEY (backend_id, stable_id) REFERENCES cached_objects (backend_id, stable_id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS directory_snapshots (
            backend_id      TEXT NOT NULL,
            dir_stable_id   TEXT NOT NULL,
            generation      INTEGER NOT NULL DEFAULT 0,
            item_count      INTEGER NOT NULL,
            snapshot_at_secs INTEGER NOT NULL,
            is_complete     INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (backend_id, dir_stable_id)
        );

        CREATE TABLE IF NOT EXISTS directory_snapshot_entries (
            backend_id       TEXT NOT NULL,
            dir_stable_id    TEXT NOT NULL,
            generation       INTEGER NOT NULL,
            stable_id         TEXT NOT NULL,
            display_path      TEXT NOT NULL,
            name              TEXT NOT NULL,
            is_dir            INTEGER NOT NULL,
            metadata_level    INTEGER NOT NULL,
            size              INTEGER,
            mtime_secs        INTEGER,
            mtime_nanos       INTEGER,
            ctime_secs        INTEGER,
            ctime_nanos       INTEGER,
            is_symlink        INTEGER NOT NULL DEFAULT 0,
            symlink_target    TEXT,
            is_inaccessible   INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (backend_id, dir_stable_id, generation, display_path)
        );

        CREATE INDEX IF NOT EXISTS idx_directory_snapshot_entries_generation
            ON directory_snapshot_entries (backend_id, dir_stable_id, generation);
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
