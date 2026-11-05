use super::conversions::{
    escape_like, i32_to_metadata_level, metadata_level_to_i32, parts_to_system_time,
    secs_to_system_time, system_time_to_parts, system_time_to_secs,
};
use super::*;

impl CatalogRepository {
    pub fn upsert_object(&self, obj: &CachedObject) -> Result<(), CatalogError> {
        self.upsert_objects(std::slice::from_ref(obj))
    }

    pub fn upsert_objects(&self, objects: &[CachedObject]) -> Result<(), CatalogError> {
        if objects.is_empty() {
            return Ok(());
        }

        let transaction = self.conn.unchecked_transaction()?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO cached_objects (backend_id, stable_id, display_path, size, mtime_secs, mtime_nanos, content_type, etag, cached_level, last_accessed_secs)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT (backend_id, stable_id) DO UPDATE SET
                    display_path = excluded.display_path,
                    size = excluded.size,
                    mtime_secs = excluded.mtime_secs,
                    mtime_nanos = excluded.mtime_nanos,
                    content_type = excluded.content_type,
                    etag = excluded.etag,
                    cached_level = excluded.cached_level,
                    last_accessed_secs = excluded.last_accessed_secs",
            )?;
            for object in objects {
                let (mtime_secs, mtime_nanos) = system_time_to_parts(object.mtime);
                statement.execute(rusqlite::params![
                    object.backend_id,
                    object.stable_id,
                    object.display_path,
                    object.size.map(|size| size as i64),
                    mtime_secs,
                    mtime_nanos,
                    object.content_type,
                    object.etag,
                    metadata_level_to_i32(&object.cached_level),
                    system_time_to_secs(object.last_accessed),
                ])?;
            }
        }
        transaction.commit()?;
        tracing::debug!(objects = objects.len(), "catalog.upsert_objects");
        Ok(())
    }

    pub fn get_object(
        &self,
        backend_id: &str,
        stable_id: &str,
    ) -> Result<Option<CachedObject>, CatalogError> {
        let mut stmt = self.conn.prepare(
            "SELECT display_path, size, mtime_secs, mtime_nanos, content_type, etag, cached_level, last_accessed_secs
             FROM cached_objects WHERE backend_id = ?1 AND stable_id = ?2",
        )?;

        let result = stmt.query_row(rusqlite::params![backend_id, stable_id], |row| {
            let size: Option<i64> = row.get(1)?;
            let mtime_secs: Option<i64> = row.get(2)?;
            let mtime_nanos: Option<i64> = row.get(3)?;
            let cached_level_i: i32 = row.get(6)?;
            let last_accessed_secs: i64 = row.get(7)?;

            Ok(CachedObject {
                backend_id: backend_id.to_string(),
                stable_id: stable_id.to_string(),
                display_path: row.get(0)?,
                size: size.map(|s| s as u64),
                mtime: parts_to_system_time(mtime_secs, mtime_nanos),
                content_type: row.get(4)?,
                etag: row.get(5)?,
                cached_level: i32_to_metadata_level(cached_level_i),
                last_accessed: secs_to_system_time(last_accessed_secs),
            })
        });

        match result {
            Ok(obj) => Ok(Some(obj)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(CatalogError::Database(e)),
        }
    }

    pub fn delete_object(&self, backend_id: &str, stable_id: &str) -> Result<bool, CatalogError> {
        let deleted = self.conn.execute(
            "DELETE FROM cached_objects WHERE backend_id = ?1 AND stable_id = ?2",
            rusqlite::params![backend_id, stable_id],
        )?;
        tracing::debug!(
            backend_id,
            stable_id_hash = %iris_core_domain::observability::path_hash(stable_id),
            deleted,
            "catalog.delete_object"
        );
        Ok(deleted > 0)
    }

    /// Display paths starting with `prefix`, for path completion.
    pub fn list_object_display_paths_by_prefix(
        &self,
        backend_id: &str,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<String>, CatalogError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut stmt = self.conn.prepare(
            "SELECT display_path
             FROM cached_objects
             WHERE backend_id = ?1
               AND display_path LIKE ?2 ESCAPE '\\'
             ORDER BY CASE WHEN display_path = ?3 THEN 0 ELSE 1 END, display_path ASC
             LIMIT ?4",
        )?;
        let pattern = format!("{}%", escape_like(prefix));
        let rows = stmt.query_map(
            rusqlite::params![backend_id, pattern, prefix, limit as i64],
            |row| row.get(0),
        )?;

        let mut paths = Vec::new();
        for row in rows {
            paths.push(row?);
        }
        Ok(paths)
    }

    /// Also removes the thumbnail files of the deleted objects.
    pub fn delete_objects_by_display_path_prefix(
        &self,
        backend_id: &str,
        prefix: &str,
    ) -> Result<Vec<String>, CatalogError> {
        let objects = self.list_objects_by_display_path_prefix(backend_id, prefix)?;
        if objects.is_empty() {
            return Ok(Vec::new());
        }

        let stable_ids = objects
            .iter()
            .map(|object| object.stable_id.clone())
            .collect::<Vec<_>>();

        for stable_id in &stable_ids {
            for entry in self.list_thumbnails(backend_id, stable_id)? {
                let _ = std::fs::remove_file(entry.cache_file_path);
            }
        }

        for stable_id in &stable_ids {
            self.conn.execute(
                "DELETE FROM cached_objects WHERE backend_id = ?1 AND stable_id = ?2",
                rusqlite::params![backend_id, stable_id],
            )?;
        }
        tracing::debug!(
            backend_id,
            prefix_hash = %iris_core_domain::observability::path_hash(prefix),
            deleted = stable_ids.len(),
            "catalog.delete_objects_by_prefix"
        );

        Ok(stable_ids)
    }

    /// Rewrites display paths under `old_prefix` after a rename or move.
    pub fn rebind_display_path_prefix(
        &self,
        backend_id: &str,
        old_prefix: &str,
        new_prefix: &str,
    ) -> Result<u64, CatalogError> {
        if old_prefix == new_prefix {
            return Ok(0);
        }

        let objects = self.list_objects_by_display_path_prefix(backend_id, old_prefix)?;
        for object in &objects {
            let suffix = object
                .display_path
                .strip_prefix(old_prefix)
                .unwrap_or_default();
            self.conn.execute(
                "UPDATE cached_objects
                 SET display_path = ?1
                 WHERE backend_id = ?2 AND stable_id = ?3",
                rusqlite::params![
                    format!("{new_prefix}{suffix}"),
                    backend_id,
                    object.stable_id,
                ],
            )?;
        }
        tracing::debug!(
            backend_id,
            old_prefix_hash = %iris_core_domain::observability::path_hash(old_prefix),
            new_prefix_hash = %iris_core_domain::observability::path_hash(new_prefix),
            updated = objects.len(),
            "catalog.rebind_display_path_prefix"
        );

        Ok(objects.len() as u64)
    }
}

impl CatalogRepository {
    fn list_objects_by_display_path_prefix(
        &self,
        backend_id: &str,
        prefix: &str,
    ) -> Result<Vec<CachedObject>, CatalogError> {
        let mut stmt = self.conn.prepare(
            "SELECT stable_id, display_path, size, mtime_secs, mtime_nanos, content_type, etag, cached_level, last_accessed_secs
             FROM cached_objects
             WHERE backend_id = ?1
               AND (display_path = ?2 OR display_path LIKE ?3 ESCAPE '\\')
             ORDER BY display_path ASC",
        )?;
        let pattern = format!("{}/%", escape_like(prefix));
        let rows = stmt.query_map(rusqlite::params![backend_id, prefix, pattern], |row| {
            let size: Option<i64> = row.get(2)?;
            let mtime_secs: Option<i64> = row.get(3)?;
            let mtime_nanos: Option<i64> = row.get(4)?;
            let cached_level_i: i32 = row.get(7)?;
            let last_accessed_secs: i64 = row.get(8)?;

            Ok(CachedObject {
                backend_id: backend_id.to_string(),
                stable_id: row.get(0)?,
                display_path: row.get(1)?,
                size: size.map(|s| s as u64),
                mtime: parts_to_system_time(mtime_secs, mtime_nanos),
                content_type: row.get(5)?,
                etag: row.get(6)?,
                cached_level: i32_to_metadata_level(cached_level_i),
                last_accessed: secs_to_system_time(last_accessed_secs),
            })
        })?;

        let mut objects = Vec::new();
        for row in rows {
            objects.push(row?);
        }
        Ok(objects)
    }
}
