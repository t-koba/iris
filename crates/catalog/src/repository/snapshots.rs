use super::conversions::{
    i32_to_metadata_level, metadata_level_to_i32, parts_to_system_time, secs_to_system_time,
    system_time_to_parts, system_time_to_secs,
};
use super::*;

/// In-progress listings live only in this connection's TEMP database, so another
/// connection's eviction cannot see or delete them. Main-DB rows are committed atomically.
const SNAPSHOT_STAGING_SCHEMA: &str = "
CREATE TEMP TABLE IF NOT EXISTS iris_snapshot_staging (
    backend_id          TEXT NOT NULL,
    dir_stable_id       TEXT NOT NULL,
    generation          INTEGER NOT NULL,
    stable_id           TEXT NOT NULL,
    display_path        TEXT NOT NULL COLLATE IRIS_PATH_IDENTITY,
    name                TEXT NOT NULL,
    is_dir              INTEGER NOT NULL,
    metadata_level      INTEGER NOT NULL,
    size                INTEGER,
    mtime_secs          INTEGER,
    mtime_nanos         INTEGER,
    ctime_secs          INTEGER,
    ctime_nanos         INTEGER,
    is_symlink          INTEGER NOT NULL DEFAULT 0,
    symlink_target      TEXT,
    is_inaccessible     INTEGER NOT NULL DEFAULT 0,
    local_path_is_lossy INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (backend_id, dir_stable_id, generation, display_path, stable_id)
);
";

impl CatalogRepository {
    pub fn upsert_snapshot(&self, snapshot: &DirectorySnapshot) -> Result<(), CatalogError> {
        let snapshot_at_secs = system_time_to_secs(snapshot.snapshot_at);
        self.conn.execute(
            "INSERT INTO directory_snapshots (backend_id, dir_stable_id, generation, item_count, snapshot_at_secs, is_complete)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (backend_id, dir_stable_id) DO UPDATE SET
                generation = CASE
                    WHEN excluded.generation = 0 THEN directory_snapshots.generation
                    ELSE excluded.generation
                END,
                item_count = excluded.item_count,
                snapshot_at_secs = excluded.snapshot_at_secs,
                is_complete = excluded.is_complete",
            rusqlite::params![
                snapshot.backend_id,
                snapshot.dir_stable_id,
                snapshot.generation as i64,
                snapshot.item_count as i64,
                snapshot_at_secs,
                snapshot.is_complete as i32,
            ],
        )?;
        tracing::debug!(
            backend_id = snapshot.backend_id,
            dir_stable_id_hash = %iris_core_domain::observability::path_hash(&snapshot.dir_stable_id),
            item_count = snapshot.item_count,
            is_complete = snapshot.is_complete,
            "catalog.upsert_snapshot"
        );
        Ok(())
    }

    pub fn get_snapshot(
        &self,
        backend_id: &str,
        dir_stable_id: &str,
    ) -> Result<Option<DirectorySnapshot>, CatalogError> {
        let mut stmt = self.conn.prepare(
            "SELECT generation, item_count, snapshot_at_secs, is_complete
             FROM directory_snapshots WHERE backend_id = ?1 AND dir_stable_id = ?2",
        )?;

        let result = stmt.query_row(rusqlite::params![backend_id, dir_stable_id], |row| {
            let generation: i64 = row.get(0)?;
            let item_count: i64 = row.get(1)?;
            let snapshot_at_secs: i64 = row.get(2)?;
            let is_complete: i32 = row.get(3)?;

            Ok(DirectorySnapshot {
                backend_id: backend_id.to_string(),
                dir_stable_id: dir_stable_id.to_string(),
                generation: generation as u64,
                item_count: item_count as u64,
                snapshot_at: secs_to_system_time(snapshot_at_secs),
                is_complete: is_complete != 0,
            })
        });

        match result {
            Ok(snap) => Ok(Some(snap)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(CatalogError::Database(e)),
        }
    }

    /// Reserve a generation by writing a sentinel row into this connection's TEMP staging.
    /// An IMMEDIATE transaction takes the write lock before reading and advancing the durable
    /// counter, so no connection can reuse a generation, even across restarts. Busy responses
    /// are retried within a bounded budget; the sentinel marks an owned, incomplete listing.
    pub fn begin_snapshot_generation(
        &mut self,
        backend_id: &str,
        dir_stable_id: &str,
    ) -> Result<u64, CatalogError> {
        for attempt in 0..8 {
            let reserved: rusqlite::Result<Option<u64>> = (|| {
                let transaction = self
                    .conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                transaction.execute_batch(SNAPSHOT_STAGING_SCHEMA)?;
                let current: i64 = transaction.query_row(
                    "SELECT MAX(generation) FROM (
                        SELECT COALESCE(MAX(next_generation), 0) AS generation FROM snapshot_generation_counter
                        UNION ALL
                        SELECT COALESCE(MAX(generation), 0) FROM directory_snapshots
                        UNION ALL
                        SELECT COALESCE(MAX(generation), 0) FROM directory_snapshot_entries
                     )",
                    [],
                    |row| row.get(0),
                )?;
                let generation = current.saturating_add(1);
                transaction.execute(
                    "UPDATE snapshot_generation_counter SET next_generation = ?1 WHERE id = 1",
                    [generation],
                )?;
                match transaction.execute(
                    "INSERT INTO iris_snapshot_staging (
                        backend_id, dir_stable_id, generation, stable_id, display_path, name,
                        is_dir, metadata_level, size, mtime_secs, mtime_nanos, ctime_secs,
                        ctime_nanos, is_symlink, symlink_target, is_inaccessible, local_path_is_lossy
                     ) VALUES (?1, ?2, ?3, ?4, '', '', 0, 0, NULL, ?5, NULL, NULL, NULL, 0, NULL, 0, 0)",
                    rusqlite::params![
                        backend_id,
                        dir_stable_id,
                        generation,
                        SNAPSHOT_RESERVATION_STABLE_ID,
                        system_time_to_secs(SystemTime::now()),
                    ],
                ) {
                    Ok(_) => {
                        transaction.commit()?;
                        Ok(Some(generation as u64))
                    }
                    // Another writer reserved this generation first; recompute and retry.
                    Err(rusqlite::Error::SqliteFailure(error, _))
                        if error.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        Ok(None)
                    }
                    Err(error) => Err(error),
                }
            })();
            match reserved {
                Ok(Some(generation)) => return Ok(generation),
                Ok(None) => continue,
                Err(rusqlite::Error::SqliteFailure(error, _))
                    if error.code == rusqlite::ErrorCode::ConstraintViolation
                        || error.code == rusqlite::ErrorCode::DatabaseBusy =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5 * (attempt + 1)));
                    continue;
                }
                Err(error) => return Err(CatalogError::Database(error)),
            }
        }
        Err(CatalogError::Database(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("could not reserve a directory snapshot generation".to_string()),
        )))
    }

    /// Discard this connection's TEMP staging for an abandoned generation. The committed
    /// generation is untouched, so readers keep seeing the last complete listing.
    pub fn abandon_snapshot_generation(
        &mut self,
        backend_id: &str,
        dir_stable_id: &str,
        generation: u64,
    ) -> Result<(), CatalogError> {
        self.conn.execute_batch(SNAPSHOT_STAGING_SCHEMA)?;
        self.conn.execute(
            "DELETE FROM iris_snapshot_staging
             WHERE backend_id = ?1 AND dir_stable_id = ?2 AND generation = ?3",
            rusqlite::params![backend_id, dir_stable_id, generation as i64],
        )?;
        Ok(())
    }

    pub fn append_snapshot_entries(
        &mut self,
        backend_id: &str,
        dir_stable_id: &str,
        generation: u64,
        entries: &[CachedDirectoryEntry],
    ) -> Result<(), CatalogError> {
        let transaction = self.conn.transaction()?;
        require_snapshot_reservation(&transaction, backend_id, dir_stable_id, generation)?;
        {
            let mut statement = transaction.prepare(
                "INSERT OR REPLACE INTO iris_snapshot_staging (
                    backend_id, dir_stable_id, generation, stable_id, display_path, name,
                    is_dir, metadata_level, size, mtime_secs, mtime_nanos, ctime_secs,
                    ctime_nanos, is_symlink, symlink_target, is_inaccessible, local_path_is_lossy
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17
                 )",
            )?;
            for entry in entries {
                let (mtime_secs, mtime_nanos) = system_time_to_parts(entry.mtime);
                let (ctime_secs, ctime_nanos) = system_time_to_parts(entry.ctime);
                statement.execute(rusqlite::params![
                    backend_id,
                    dir_stable_id,
                    generation as i64,
                    entry.stable_id,
                    entry.display_path,
                    entry.name,
                    entry.is_dir as i32,
                    metadata_level_to_i32(&entry.metadata_level),
                    entry.size.map(|size| size as i64),
                    mtime_secs,
                    mtime_nanos,
                    ctime_secs,
                    ctime_nanos,
                    entry.is_symlink as i32,
                    entry.symlink_target,
                    entry.is_inaccessible as i32,
                    entry.local_path_is_lossy as i32,
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn complete_snapshot_generation(
        &mut self,
        backend_id: &str,
        dir_stable_id: &str,
        generation: u64,
    ) -> Result<DirectorySnapshot, CatalogError> {
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        require_snapshot_reservation(&transaction, backend_id, dir_stable_id, generation)?;
        transaction.execute(
            "DELETE FROM iris_snapshot_staging
             WHERE backend_id = ?1 AND dir_stable_id = ?2 AND generation = ?3
               AND stable_id = ?4",
            rusqlite::params![
                backend_id,
                dir_stable_id,
                generation as i64,
                SNAPSHOT_RESERVATION_STABLE_ID,
            ],
        )?;
        let item_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM iris_snapshot_staging
             WHERE backend_id = ?1 AND dir_stable_id = ?2 AND generation = ?3",
            rusqlite::params![backend_id, dir_stable_id, generation as i64],
            |row| row.get(0),
        )?;
        let snapshot = DirectorySnapshot {
            backend_id: backend_id.to_string(),
            dir_stable_id: dir_stable_id.to_string(),
            generation,
            item_count: item_count as u64,
            snapshot_at: SystemTime::now(),
            is_complete: true,
        };
        let snapshot_at_secs = system_time_to_secs(snapshot.snapshot_at);
        transaction.execute(
            "INSERT INTO directory_snapshots (
                backend_id, dir_stable_id, generation, item_count, snapshot_at_secs, is_complete
             ) VALUES (?1, ?2, ?3, ?4, ?5, 1)
             ON CONFLICT (backend_id, dir_stable_id) DO UPDATE SET
                generation = excluded.generation,
                item_count = excluded.item_count,
                snapshot_at_secs = excluded.snapshot_at_secs,
                is_complete = 1",
            rusqlite::params![
                backend_id,
                dir_stable_id,
                generation as i64,
                item_count,
                snapshot_at_secs,
            ],
        )?;
        transaction.execute(
            "DELETE FROM directory_snapshot_entries
             WHERE backend_id = ?1 AND dir_stable_id = ?2 AND generation != ?3",
            rusqlite::params![backend_id, dir_stable_id, generation as i64],
        )?;
        transaction.execute(
            "INSERT INTO directory_snapshot_entries (
                backend_id, dir_stable_id, generation, stable_id, display_path, name,
                is_dir, metadata_level, size, mtime_secs, mtime_nanos, ctime_secs,
                ctime_nanos, is_symlink, symlink_target, is_inaccessible, local_path_is_lossy
             )
             SELECT backend_id, dir_stable_id, generation, stable_id, display_path, name,
                    is_dir, metadata_level, size, mtime_secs, mtime_nanos, ctime_secs,
                    ctime_nanos, is_symlink, symlink_target, is_inaccessible, local_path_is_lossy
             FROM iris_snapshot_staging
             WHERE backend_id = ?1 AND dir_stable_id = ?2 AND generation = ?3
             ORDER BY rowid",
            rusqlite::params![backend_id, dir_stable_id, generation as i64],
        )?;
        transaction.commit()?;
        // Only a committed generation may lose its connection-local reservation.
        self.conn.execute(
            "DELETE FROM iris_snapshot_staging
             WHERE backend_id = ?1 AND dir_stable_id = ?2 AND generation = ?3",
            rusqlite::params![backend_id, dir_stable_id, generation as i64],
        )?;
        Ok(snapshot)
    }

    pub fn get_snapshot_entries(
        &self,
        backend_id: &str,
        dir_stable_id: &str,
    ) -> Result<Option<Vec<CachedDirectoryEntry>>, CatalogError> {
        let Some(snapshot) = self.get_snapshot(backend_id, dir_stable_id)? else {
            return Ok(None);
        };
        if !snapshot.is_complete {
            return Ok(None);
        }
        let mut statement = self.conn.prepare(
            "SELECT stable_id, display_path, name, is_dir, metadata_level, size,
                    mtime_secs, mtime_nanos, ctime_secs, ctime_nanos, is_symlink,
                    symlink_target, is_inaccessible, local_path_is_lossy
             FROM directory_snapshot_entries
             WHERE backend_id = ?1 AND dir_stable_id = ?2 AND generation = ?3
             ORDER BY rowid",
        )?;
        let rows = statement.query_map(
            rusqlite::params![backend_id, dir_stable_id, snapshot.generation as i64],
            |row| {
                let size: Option<i64> = row.get(5)?;
                let metadata_level: i32 = row.get(4)?;
                Ok(CachedDirectoryEntry {
                    local_path_is_lossy: row.get::<_, i32>(13)? != 0,
                    stable_id: row.get(0)?,
                    display_path: row.get(1)?,
                    name: row.get(2)?,
                    is_dir: row.get::<_, i32>(3)? != 0,
                    metadata_level: i32_to_metadata_level(metadata_level),
                    size: size.map(|value| value as u64),
                    mtime: parts_to_system_time(row.get(6)?, row.get(7)?),
                    ctime: parts_to_system_time(row.get(8)?, row.get(9)?),
                    is_symlink: row.get::<_, i32>(10)? != 0,
                    symlink_target: row.get(11)?,
                    is_inaccessible: row.get::<_, i32>(12)? != 0,
                })
            },
        )?;
        let mut entries = Vec::with_capacity(snapshot.item_count as usize);
        for row in rows {
            entries.push(row?);
        }
        if entries.len() as u64 != snapshot.item_count {
            return Ok(None);
        }
        Ok(Some(entries))
    }
}

/// Check inside the write transaction so an abandoned reservation cannot be resurrected.
fn require_snapshot_reservation(
    transaction: &rusqlite::Transaction<'_>,
    backend_id: &str,
    dir_stable_id: &str,
    generation: u64,
) -> Result<(), CatalogError> {
    transaction.execute_batch(SNAPSHOT_STAGING_SCHEMA)?;
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM iris_snapshot_staging
         WHERE backend_id = ?1 AND dir_stable_id = ?2 AND generation = ?3 AND stable_id = ?4)",
        rusqlite::params![
            backend_id,
            dir_stable_id,
            generation as i64,
            SNAPSHOT_RESERVATION_STABLE_ID,
        ],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(CatalogError::SnapshotReservationMissing { generation });
    }
    Ok(())
}
