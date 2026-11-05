use rusqlite::Connection;
use std::path::PathBuf;
use std::time::Duration;

/// LRU, size-limit and TTL eviction for the catalog.
pub struct CachePolicy {
    pub max_entries: u64,
    pub max_thumbnail_bytes: u64,
    pub ttl: Duration,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_thumbnail_bytes: 1_024 * 1_024 * 1_024, // 1 GB
            ttl: Duration::from_secs(30 * 24 * 3600),   // 30 days
        }
    }
}

impl CachePolicy {
    pub fn evict_expired(&self, conn: &Connection) -> Result<u64, rusqlite::Error> {
        let cutoff_secs = now_secs() - self.ttl.as_secs() as i64;
        delete_thumbnails_for_object_query(
            conn,
            "SELECT backend_id, stable_id FROM cached_objects WHERE last_accessed_secs < ?1",
            [cutoff_secs],
        )?;
        let deleted = conn.execute(
            "DELETE FROM cached_objects WHERE last_accessed_secs < ?1",
            [cutoff_secs],
        )?;
        Ok(deleted as u64)
    }

    pub fn evict_lru(&self, conn: &Connection) -> Result<u64, rusqlite::Error> {
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM cached_objects", [], |row| row.get(0))?;
        if count <= self.max_entries as i64 {
            return Ok(0);
        }
        let excess = count - self.max_entries as i64;
        delete_thumbnails_for_object_query(
            conn,
            "SELECT backend_id, stable_id FROM cached_objects ORDER BY last_accessed_secs ASC LIMIT ?1",
            [excess],
        )?;
        let deleted = conn.execute(
            "DELETE FROM cached_objects WHERE rowid IN (
                SELECT rowid FROM cached_objects ORDER BY last_accessed_secs ASC LIMIT ?1
            )",
            [excess],
        )?;
        Ok(deleted as u64)
    }

    pub fn evict_thumbnail_bytes(&self, conn: &Connection) -> Result<u64, rusqlite::Error> {
        let total_bytes: i64 = conn.query_row(
            "SELECT COALESCE(SUM(byte_size), 0) FROM thumbnail_entries",
            [],
            |row| row.get(0),
        )?;

        if total_bytes <= self.max_thumbnail_bytes as i64 {
            return Ok(0);
        }

        let mut reclaimed = 0i64;
        let mut to_delete = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT backend_id, stable_id, dimension, cache_file_path, byte_size
             FROM thumbnail_entries
             ORDER BY generated_at_secs ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;

        for row in rows {
            let (backend_id, stable_id, dimension, cache_file_path, byte_size) = row?;
            to_delete.push((
                backend_id,
                stable_id,
                dimension,
                PathBuf::from(cache_file_path),
            ));
            reclaimed += byte_size.max(0);
            if total_bytes - reclaimed <= self.max_thumbnail_bytes as i64 {
                break;
            }
        }

        for (_, _, _, cache_file_path) in &to_delete {
            let _ = std::fs::remove_file(cache_file_path);
        }

        let mut deleted = 0u64;
        for (backend_id, stable_id, dimension, _) in to_delete {
            deleted += conn.execute(
                "DELETE FROM thumbnail_entries
                 WHERE backend_id = ?1 AND stable_id = ?2 AND dimension = ?3",
                rusqlite::params![backend_id, stable_id, dimension],
            )? as u64;
        }

        Ok(deleted)
    }

    pub fn run_eviction(&self, conn: &Connection) -> Result<u64, rusqlite::Error> {
        let expired = self.evict_expired(conn)?;
        let lru = self.evict_lru(conn)?;
        let thumbnails = self.evict_thumbnail_bytes(conn)?;
        Ok(expired + lru + thumbnails)
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn delete_thumbnails_for_object_query<P>(
    conn: &Connection,
    object_query: &str,
    params: P,
) -> Result<(), rusqlite::Error>
where
    P: rusqlite::Params,
{
    let query = format!(
        "SELECT cache_file_path
         FROM thumbnail_entries
         WHERE (backend_id, stable_id) IN ({})",
        object_query
    );

    let mut stmt = conn.prepare(&query)?;
    let rows = stmt.query_map(params, |row| row.get::<_, String>(0))?;
    for row in rows {
        let path = PathBuf::from(row?);
        let _ = std::fs::remove_file(path);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use tempfile::tempdir;

    #[test]
    fn evict_thumbnail_bytes_removes_files_and_rows() {
        let conn = schema::open(None, 0).expect("open in-memory sqlite");

        let dir = tempdir().expect("create tempdir");
        let file_path = dir.path().join("thumb.jpg");
        std::fs::write(&file_path, vec![7u8; 16]).expect("write thumbnail");

        conn.execute(
            "INSERT INTO cached_objects (backend_id, stable_id, display_path, cached_level, last_accessed_secs)
             VALUES ('local', 'id-1', '/tmp/id-1', 1, 1)",
            [],
        )
        .expect("insert cached object");
        conn.execute(
            "INSERT INTO thumbnail_entries (backend_id, stable_id, dimension, format, cache_file_path, byte_size, generated_at_secs, source_etag)
             VALUES ('local', 'id-1', 256, 'jpeg', ?1, 16, 1, NULL)",
            [file_path.to_string_lossy().to_string()],
        )
        .expect("insert thumbnail entry");

        let policy = CachePolicy {
            max_entries: 100,
            max_thumbnail_bytes: 0,
            ttl: Duration::from_secs(60),
        };
        let deleted = policy
            .evict_thumbnail_bytes(&conn)
            .expect("evict thumbnail bytes");

        assert_eq!(deleted, 1);
        assert!(!file_path.exists());
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM thumbnail_entries", [], |row| {
                row.get(0)
            })
            .expect("query remaining rows");
        assert_eq!(remaining, 0);
    }
}
