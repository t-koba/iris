use super::conversions::{
    image_format_to_str, parts_to_system_time, secs_to_system_time, str_to_image_format,
    system_time_to_parts, system_time_to_secs,
};
use super::*;

impl CatalogRepository {
    pub fn upsert_thumbnail(&self, entry: &ThumbnailEntry) -> Result<(), CatalogError> {
        let format_str = image_format_to_str(&entry.format);
        let generated_at_secs = system_time_to_secs(entry.generated_at);
        let (source_mtime_secs, source_mtime_nanos) = system_time_to_parts(entry.source_mtime);

        self.conn.execute(
            "INSERT INTO thumbnail_entries (backend_id, stable_id, dimension, format, cache_file_path, byte_size, generated_at_secs, source_size, source_mtime_secs, source_mtime_nanos, source_etag)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (backend_id, stable_id, dimension) DO UPDATE SET
                format = excluded.format,
                cache_file_path = excluded.cache_file_path,
                byte_size = excluded.byte_size,
                generated_at_secs = excluded.generated_at_secs,
                source_size = excluded.source_size,
                source_mtime_secs = excluded.source_mtime_secs,
                source_mtime_nanos = excluded.source_mtime_nanos,
                source_etag = excluded.source_etag",
            rusqlite::params![
                entry.backend_id,
                entry.stable_id,
                entry.dimension,
                format_str,
                entry.cache_file_path.to_string_lossy().to_string(),
                entry.byte_size as i64,
                generated_at_secs,
                entry.source_size.map(|size| size as i64),
                source_mtime_secs,
                source_mtime_nanos,
                entry.source_etag,
            ],
        )?;
        tracing::debug!(
            backend_id = entry.backend_id,
            stable_id_hash = %iris_core_domain::observability::path_hash(&entry.stable_id),
            dimension = entry.dimension,
            bytes = entry.byte_size,
            "catalog.upsert_thumbnail"
        );
        Ok(())
    }

    pub fn get_thumbnail(
        &self,
        backend_id: &str,
        stable_id: &str,
        dimension: u32,
    ) -> Result<Option<ThumbnailEntry>, CatalogError> {
        let mut stmt = self.conn.prepare(
            "SELECT format, cache_file_path, byte_size, generated_at_secs, source_size, source_mtime_secs, source_mtime_nanos, source_etag
             FROM thumbnail_entries WHERE backend_id = ?1 AND stable_id = ?2 AND dimension = ?3",
        )?;

        let result = stmt.query_row(rusqlite::params![backend_id, stable_id, dimension], |row| {
            let format_str: String = row.get(0)?;
            let path_str: String = row.get(1)?;
            let byte_size: i64 = row.get(2)?;
            let generated_at_secs: i64 = row.get(3)?;
            let source_size: Option<i64> = row.get(4)?;
            let source_mtime_secs: Option<i64> = row.get(5)?;
            let source_mtime_nanos: Option<i64> = row.get(6)?;

            Ok(ThumbnailEntry {
                backend_id: backend_id.to_string(),
                stable_id: stable_id.to_string(),
                dimension,
                format: str_to_image_format(&format_str),
                cache_file_path: PathBuf::from(path_str),
                byte_size: byte_size as u64,
                generated_at: secs_to_system_time(generated_at_secs),
                source_size: source_size.map(|size| size as u64),
                source_mtime: parts_to_system_time(source_mtime_secs, source_mtime_nanos),
                source_etag: row.get(7)?,
            })
        });

        match result {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(CatalogError::Database(e)),
        }
    }

    pub fn list_thumbnails(
        &self,
        backend_id: &str,
        stable_id: &str,
    ) -> Result<Vec<ThumbnailEntry>, CatalogError> {
        let mut stmt = self.conn.prepare(
            "SELECT dimension, format, cache_file_path, byte_size, generated_at_secs, source_size, source_mtime_secs, source_mtime_nanos, source_etag
             FROM thumbnail_entries WHERE backend_id = ?1 AND stable_id = ?2",
        )?;

        let rows = stmt.query_map(rusqlite::params![backend_id, stable_id], |row| {
            let format_str: String = row.get(1)?;
            let path_str: String = row.get(2)?;
            let byte_size: i64 = row.get(3)?;
            let generated_at_secs: i64 = row.get(4)?;
            let source_size: Option<i64> = row.get(5)?;
            let source_mtime_secs: Option<i64> = row.get(6)?;
            let source_mtime_nanos: Option<i64> = row.get(7)?;

            Ok(ThumbnailEntry {
                backend_id: backend_id.to_string(),
                stable_id: stable_id.to_string(),
                dimension: row.get(0)?,
                format: str_to_image_format(&format_str),
                cache_file_path: PathBuf::from(path_str),
                byte_size: byte_size as u64,
                generated_at: secs_to_system_time(generated_at_secs),
                source_size: source_size.map(|size| size as u64),
                source_mtime: parts_to_system_time(source_mtime_secs, source_mtime_nanos),
                source_etag: row.get(8)?,
            })
        })?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }
}
