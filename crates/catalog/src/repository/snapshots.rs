use super::conversions::{
    i32_to_metadata_level, metadata_level_to_i32, parts_to_system_time, secs_to_system_time,
    system_time_to_parts, system_time_to_secs,
};
use super::*;

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

    pub fn begin_snapshot_generation(
        &mut self,
        backend_id: &str,
        dir_stable_id: &str,
    ) -> Result<u64, CatalogError> {
        let transaction = self.conn.transaction()?;
        let current: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(generation), 0) FROM (
                SELECT generation FROM directory_snapshots WHERE backend_id = ?1 AND dir_stable_id = ?2
                UNION ALL
                SELECT generation FROM directory_snapshot_entries WHERE backend_id = ?1 AND dir_stable_id = ?2
            )",
            rusqlite::params![backend_id, dir_stable_id],
            |row| row.get(0),
        )?;
        let generation = current.saturating_add(1);
        transaction.execute(
            "DELETE FROM directory_snapshot_entries
             WHERE backend_id = ?1 AND dir_stable_id = ?2 AND generation != COALESCE(
                 (SELECT generation FROM directory_snapshots WHERE backend_id = ?1 AND dir_stable_id = ?2),
                 -1
             )",
            rusqlite::params![backend_id, dir_stable_id],
        )?;
        transaction.commit()?;
        Ok(generation as u64)
    }

    pub fn append_snapshot_entries(
        &mut self,
        backend_id: &str,
        dir_stable_id: &str,
        generation: u64,
        entries: &[CachedDirectoryEntry],
    ) -> Result<(), CatalogError> {
        if entries.is_empty() {
            return Ok(());
        }
        let transaction = self.conn.transaction()?;
        {
            let mut statement = transaction.prepare(
                "INSERT OR REPLACE INTO directory_snapshot_entries (
                    backend_id, dir_stable_id, generation, stable_id, display_path, name,
                    is_dir, metadata_level, size, mtime_secs, mtime_nanos, ctime_secs,
                    ctime_nanos, is_symlink, symlink_target, is_inaccessible
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16
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
        let transaction = self.conn.transaction()?;
        let item_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM directory_snapshot_entries
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
        transaction.commit()?;
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
                    symlink_target, is_inaccessible
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
