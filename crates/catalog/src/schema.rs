use rusqlite::Connection;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: i64 = 4;

const SCHEMA: &str = "
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
    display_path      TEXT NOT NULL COLLATE IRIS_PATH_IDENTITY,
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
    local_path_is_lossy INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (backend_id, dir_stable_id, generation, display_path, stable_id)
);

CREATE INDEX IF NOT EXISTS idx_directory_snapshot_entries_generation
    ON directory_snapshot_entries (backend_id, dir_stable_id, generation);

CREATE TABLE IF NOT EXISTS snapshot_generation_counter (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    next_generation INTEGER NOT NULL
);

INSERT OR IGNORE INTO snapshot_generation_counter (id, next_generation) VALUES (1, 0);
";

/// A database with a different schema version is a disposable cache; it is rebuilt.
/// Retire its owned thumbnail files first so rebuilding cannot lose their ownership.
pub fn open(path: Option<&Path>, mmap_bytes: u64) -> rusqlite::Result<Connection> {
    let owned = path
        .map(|path| {
            real_directory(
                &path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join("thumbnail-cache"),
            )
        })
        .transpose()
        .map_err(io_error)?
        .flatten();
    if let (Some(path), Some(owned)) = (path, owned.as_deref()) {
        retire_before_rebuild(path, owned)?;
    }
    let conn = iris_core_domain::sqlite::open(path, mmap_bytes, SCHEMA_VERSION, SCHEMA)?;
    initialize_generation_counter(&conn)?;
    if let Some(owned) = owned {
        // Failed deletions remain owned by this directory and are retried on every open,
        // including opens where the schema already has the current version.
        reclaim_retired(&owned);
    }
    Ok(conn)
}

/// A zero counter means the table was just created (or restored on an older cache).
/// Seed it once from the highest generation already present; afterwards the counter
/// is authoritative and reservations never scan the snapshot tables.
fn initialize_generation_counter(conn: &Connection) -> rusqlite::Result<()> {
    let current: i64 = conn.query_row(
        "SELECT next_generation FROM snapshot_generation_counter WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    if current != 0 {
        return Ok(());
    }
    let highest: i64 = conn.query_row(
        "SELECT COALESCE(MAX(generation), 0) FROM (
            SELECT generation FROM directory_snapshots
            UNION ALL
            SELECT generation FROM directory_snapshot_entries
         )",
        [],
        |row| row.get(0),
    )?;
    conn.execute(
        "UPDATE snapshot_generation_counter SET next_generation = ?1
         WHERE id = 1 AND next_generation = 0",
        [highest],
    )?;
    Ok(())
}

fn io_error(error: io::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

/// Do not follow a cache/retirement directory symlink to someone else's files.
fn real_directory(path: &Path) -> io::Result<Option<PathBuf>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::canonicalize(path).map(Some),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn retire_before_rebuild(db_path: &Path, owned: &Path) -> rusqlite::Result<()> {
    if !db_path.try_exists().map_err(io_error)? {
        return Ok(());
    }
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == SCHEMA_VERSION
        || !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'thumbnail_entries')",
            [],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Ok(());
    }
    let mut stmt = conn.prepare("SELECT cache_file_path FROM thumbnail_entries")?;
    for path in stmt.query_map([], |row| row.get::<_, String>(0))? {
        retire_file(Path::new(&path?), owned).map_err(io_error)?;
    }
    Ok(())
}

fn retire_file(path: &Path, owned: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(()), // Never move symlinks or directories from SQL rows.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let path = fs::canonicalize(path)?;
    let retired = owned.join(".retired");
    if !path.starts_with(owned) || path.starts_with(&retired) {
        return Ok(());
    }
    match fs::create_dir(&retired) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if real_directory(&retired)?.as_deref() != Some(retired.as_path()) {
                return Err(io::Error::other(
                    "thumbnail retirement path is not a real directory",
                ));
            }
        }
        Err(error) => return Err(error),
    }
    // Reserve a unique filename, then replace only our own empty placeholder.
    // Closing its handle before rename also permits replacement on Windows.
    let destination = tempfile::NamedTempFile::new_in(&retired)?.into_temp_path();
    fs::rename(path, &destination)?;
    destination.keep().map_err(|error| error.error)?;
    Ok(())
}

fn reclaim_retired(owned: &Path) {
    let retired = owned.join(".retired");
    if !matches!(real_directory(&retired), Ok(Some(_))) {
        return;
    }
    let Ok(entries) = fs::read_dir(&retired) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_file()) {
            if let Err(error) = fs::remove_file(entry.path()) {
                tracing::warn!(%error, "thumbnail retirement deletion will be retried");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_spellings(conn: &Connection) -> Vec<String> {
        for path in ["Café.jpg", "Cafe\u{301}.jpg"] {
            conn.execute(
                "INSERT OR REPLACE INTO directory_snapshot_entries
                 (backend_id, dir_stable_id, generation, stable_id, display_path, name,
                  is_dir, metadata_level)
                 VALUES ('local', 'dir', 1, 'file', ?1, ?1, 0, 0)",
                [path],
            )
            .unwrap();
        }
        conn.prepare("SELECT display_path FROM directory_snapshot_entries ORDER BY display_path")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn snapshot_path_collation_uses_byte_equality_on_every_platform() {
        let conn = open(None, 0).unwrap();
        let paths = insert_spellings(&conn);
        assert_eq!(paths, ["Cafe\u{301}.jpg", "Café.jpg"]);
        // The named collation itself must not match a canonically equivalent respelling.
        let matches = conn
            .prepare(
                "SELECT display_path FROM directory_snapshot_entries
                 WHERE display_path = ?1 COLLATE IRIS_PATH_IDENTITY",
            )
            .unwrap()
            .query_map(["Café.jpg"], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(matches, ["Café.jpg"]);
    }

    fn old_catalog(db: &Path, paths: &[&Path]) {
        let conn = Connection::open(db).unwrap();
        conn.execute_batch(
            "PRAGMA user_version = 1;
             CREATE TABLE thumbnail_entries (cache_file_path TEXT NOT NULL);",
        )
        .unwrap();
        for path in paths {
            conn.execute(
                "INSERT INTO thumbnail_entries VALUES (?1)",
                [path.to_str().unwrap()],
            )
            .unwrap();
        }
    }

    fn version(conn: &Connection) -> i64 {
        conn.pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn generation_counter_survives_reopen_and_is_restored_on_old_caches() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("catalog.sqlite3");
        let conn = open(Some(&db), 0).unwrap();
        conn.execute(
            "UPDATE snapshot_generation_counter SET next_generation = 9 WHERE id = 1",
            [],
        )
        .unwrap();
        drop(conn);

        let conn = open(Some(&db), 0).unwrap();
        assert_eq!(version(&conn), SCHEMA_VERSION);
        let next: i64 = conn
            .query_row(
                "SELECT next_generation FROM snapshot_generation_counter WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(next, 9);

        // A cache written before the counter existed keeps its version but gains
        // the table, seeded from the highest generation already stored.
        conn.execute(
            "INSERT INTO directory_snapshots
                (backend_id, dir_stable_id, generation, item_count, snapshot_at_secs, is_complete)
             VALUES ('local', 'root-id', 100, 0, 0, 1)",
            [],
        )
        .unwrap();
        conn.execute("DROP TABLE snapshot_generation_counter", [])
            .unwrap();
        drop(conn);
        let conn = open(Some(&db), 0).unwrap();
        let next: i64 = conn
            .query_row(
                "SELECT next_generation FROM snapshot_generation_counter WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(next, 100);
    }

    #[test]
    fn rebuild_reclaims_only_owned_thumbnails_and_preserves_caching() {
        let runtime = tempfile::tempdir().unwrap();
        let db = runtime.path().join("catalog.sqlite3");
        let cache = runtime.path().join("thumbnail-cache");
        fs::create_dir(&cache).unwrap();
        let owned = cache.join("old.jpg");
        let outside = runtime.path().join("unrelated.jpg");
        let unindexed = cache.join("unindexed.jpg");
        for path in [&owned, &outside, &unindexed] {
            fs::write(path, b"thumbnail").unwrap();
        }
        // Also exercise missing and duplicate rows, and a lexical prefix sibling.
        let sibling = runtime.path().join("thumbnail-cache-other");
        fs::create_dir(&sibling).unwrap();
        let sibling_file = sibling.join("unrelated.jpg");
        fs::write(&sibling_file, b"unrelated").unwrap();
        old_catalog(
            &db,
            &[
                &owned,
                &owned,
                &cache.join("missing.jpg"),
                &outside,
                &sibling_file,
                &cache,
            ],
        );

        let conn = open(Some(&db), 0).unwrap();
        assert_eq!(version(&conn), SCHEMA_VERSION);
        assert!(!owned.exists());
        assert_eq!(fs::read(&outside).unwrap(), b"thumbnail");
        assert_eq!(fs::read(&unindexed).unwrap(), b"thumbnail");
        assert_eq!(fs::read(&sibling_file).unwrap(), b"unrelated");
        assert_eq!(fs::read_dir(cache.join(".retired")).unwrap().count(), 0);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM thumbnail_entries", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );

        let fresh = cache.join("new.jpg");
        fs::write(&fresh, b"new").unwrap();
        conn.execute_batch(
            "INSERT INTO cached_objects (backend_id, stable_id, display_path, last_accessed_secs)
             VALUES ('local', 'new', 'new.jpg', 1);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO thumbnail_entries
             (backend_id, stable_id, dimension, format, cache_file_path, byte_size, generated_at_secs)
             VALUES ('local', 'new', 256, 'jpeg', ?1, 3, 1)",
            [fresh.to_str().unwrap()],
        ).unwrap();
        // An ordinary reopen must retain the new generation, and eviction still owns it.
        drop(conn);
        let conn = open(Some(&db), 0).unwrap();
        assert_eq!(fs::read(&fresh).unwrap(), b"new");
        let policy = crate::cache_policy::CachePolicy {
            max_thumbnail_bytes: 0,
            ..Default::default()
        };
        assert_eq!(policy.evict_thumbnail_bytes(&conn).unwrap(), 1);
        assert!(!fresh.exists());
    }

    #[test]
    fn failed_retirement_preserves_rows_until_retry() {
        let runtime = tempfile::tempdir().unwrap();
        let db = runtime.path().join("catalog.sqlite3");
        let cache = runtime.path().join("thumbnail-cache");
        fs::create_dir(&cache).unwrap();
        let owned = cache.join("old.jpg");
        fs::write(&owned, b"old").unwrap();
        old_catalog(&db, &[&owned]);
        // A file blocking the retirement directory fails regardless of permissions/root.
        let retired = cache.join(".retired");
        fs::write(&retired, b"obstacle").unwrap();
        assert!(open(Some(&db), 0).is_err());
        let conn = Connection::open(&db).unwrap();
        assert_eq!(version(&conn), 1);
        assert_eq!(
            conn.query_row("SELECT cache_file_path FROM thumbnail_entries", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
            owned.to_str().unwrap()
        );
        assert_eq!(fs::read(&owned).unwrap(), b"old");
        drop(conn);
        fs::remove_file(retired).unwrap();
        assert_eq!(version(&open(Some(&db), 0).unwrap()), SCHEMA_VERSION);
        assert!(!owned.exists());
    }

    #[test]
    fn reopens_reclaim_pending_retirement_without_removing_directories() {
        let runtime = tempfile::tempdir().unwrap();
        let db = runtime.path().join("catalog.sqlite3");
        drop(open(Some(&db), 0).unwrap());
        let retired = runtime.path().join("thumbnail-cache/.retired");
        let nested = retired.join("keep-directory");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("keep-file"), b"keep").unwrap();
        let pending = retired.join("pending");
        fs::write(&pending, b"retry").unwrap();
        drop(open(Some(&db), 0).unwrap());
        assert!(!pending.exists());
        assert_eq!(fs::read(nested.join("keep-file")).unwrap(), b"keep");
    }

    #[test]
    fn absent_old_thumbnail_table_and_in_memory_opens_work() {
        assert_eq!(version(&open(None, 0).unwrap()), SCHEMA_VERSION);
        let runtime = tempfile::tempdir().unwrap();
        let db = runtime.path().join("catalog.sqlite3");
        fs::create_dir(runtime.path().join("thumbnail-cache")).unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA user_version = 1; CREATE TABLE obsolete (id INTEGER);")
            .unwrap();
        drop(conn);
        let conn = open(Some(&db), 0).unwrap();
        assert_eq!(version(&conn), SCHEMA_VERSION);
        assert!(conn.prepare("SELECT * FROM obsolete").is_err());
        assert!(conn.prepare("SELECT * FROM thumbnail_entries").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_escaping_paths_are_not_retired() {
        use std::os::unix::fs::symlink;

        let runtime = tempfile::tempdir().unwrap();
        let db = runtime.path().join("catalog.sqlite3");
        let cache = runtime.path().join("thumbnail-cache");
        fs::create_dir(&cache).unwrap();
        let outside = runtime.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let file = outside.join("keep.jpg");
        fs::write(&file, b"keep").unwrap();
        let link = cache.join("link.jpg");
        let directory_link = cache.join("escape");
        symlink(&file, &link).unwrap();
        symlink(&outside, &directory_link).unwrap();
        old_catalog(
            &db,
            &[
                &link,
                &directory_link.join("keep.jpg"),
                &cache.join("../outside/keep.jpg"),
            ],
        );
        drop(open(Some(&db), 0).unwrap());
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&file).unwrap(), b"keep");

        // A retirement-directory symlink must neither receive moves nor be cleaned.
        symlink(&outside, cache.join(".retired")).unwrap();
        drop(open(Some(&db), 0).unwrap());
        assert_eq!(fs::read(&file).unwrap(), b"keep");
        let owned = cache.join("old.jpg");
        fs::write(&owned, b"old").unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute("INSERT INTO cached_objects (backend_id, stable_id, display_path, last_accessed_secs) VALUES ('local', 'old', 'old.jpg', 1)", []).unwrap();
        conn.execute("INSERT INTO thumbnail_entries (backend_id, stable_id, dimension, format, cache_file_path, generated_at_secs) VALUES ('local', 'old', 256, 'jpeg', ?1, 1)", [owned.to_str().unwrap()]).unwrap();
        drop(conn);
        assert!(open(Some(&db), 0).is_err());
        assert!(owned.exists());
        assert_eq!(fs::read(&file).unwrap(), b"keep");
    }
}
