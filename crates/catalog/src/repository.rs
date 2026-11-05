use crate::schema;
use iris_core_domain::types::metadata::MetadataLevel;
use iris_core_domain::types::thumbnail::ImageFormat;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

mod connection;
mod conversions;
mod objects;
mod snapshots;
mod thumbnails;

use connection::configure_connection;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug)]
pub struct CachedObject {
    pub backend_id: String,
    pub stable_id: String,
    pub display_path: String,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub cached_level: MetadataLevel,
    pub last_accessed: SystemTime,
}

#[derive(Clone, Debug)]
pub struct ThumbnailEntry {
    pub backend_id: String,
    pub stable_id: String,
    pub dimension: u32,
    pub format: ImageFormat,
    pub cache_file_path: PathBuf,
    pub byte_size: u64,
    pub generated_at: SystemTime,
    pub source_size: Option<u64>,
    pub source_mtime: Option<SystemTime>,
    pub source_etag: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DirectorySnapshot {
    pub backend_id: String,
    pub dir_stable_id: String,
    pub generation: u64,
    pub item_count: u64,
    pub snapshot_at: SystemTime,
    pub is_complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedDirectoryEntry {
    pub stable_id: String,
    pub display_path: String,
    pub name: String,
    pub is_dir: bool,
    pub metadata_level: MetadataLevel,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,
    pub ctime: Option<SystemTime>,
    pub is_symlink: bool,
    pub symlink_target: Option<String>,
    pub is_inaccessible: bool,
}

/// SQLite cache index for listings and thumbnails.
pub struct CatalogRepository {
    conn: Connection,
}

impl CatalogRepository {
    pub fn open(db_path: &Path) -> Result<Self, CatalogError> {
        let conn = Connection::open(db_path)?;
        configure_connection(&conn, true)?;
        schema::initialize_schema(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, CatalogError> {
        let conn = Connection::open_in_memory()?;
        configure_connection(&conn, false)?;
        schema::initialize_schema(&conn)?;
        Ok(Self { conn })
    }

    pub fn evict(&self) -> Result<u64, CatalogError> {
        Ok(crate::cache_policy::CachePolicy::default().run_eviction(&self.conn)?)
    }

    /// Recreates the database so browsing continues after cache corruption.
    pub fn rebuild(db_path: &Path) -> Result<Self, CatalogError> {
        if db_path.exists() {
            std::fs::remove_file(db_path)?;
        }
        Self::open(db_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_upsert_and_get_object() {
        let repo = CatalogRepository::open_in_memory().unwrap();

        let obj = CachedObject {
            backend_id: "local-1".to_string(),
            stable_id: "inode-123".to_string(),
            display_path: "photos/img001.jpg".to_string(),
            size: Some(1024),
            mtime: Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1700000000)),
            content_type: Some("image/jpeg".to_string()),
            etag: None,
            cached_level: MetadataLevel::L1,
            last_accessed: SystemTime::now(),
        };

        repo.upsert_object(&obj).unwrap();
        let loaded = repo.get_object("local-1", "inode-123").unwrap().unwrap();
        assert_eq!(loaded.display_path, "photos/img001.jpg");
        assert_eq!(loaded.size, Some(1024));
    }

    #[test]
    fn batch_upsert_persists_all_objects_and_updates_existing_rows() {
        let repo = CatalogRepository::open_in_memory().unwrap();
        let object = |stable_id: &str, size: u64| CachedObject {
            backend_id: "remote".to_string(),
            stable_id: stable_id.to_string(),
            display_path: format!("folder/{stable_id}"),
            size: Some(size),
            mtime: None,
            content_type: None,
            etag: None,
            cached_level: MetadataLevel::L1,
            last_accessed: SystemTime::now(),
        };

        repo.upsert_objects(&[object("one", 1), object("two", 2)])
            .unwrap();
        repo.upsert_objects(&[object("one", 10)]).unwrap();

        assert_eq!(
            repo.get_object("remote", "one").unwrap().unwrap().size,
            Some(10)
        );
        assert_eq!(
            repo.get_object("remote", "two").unwrap().unwrap().size,
            Some(2)
        );
    }

    #[test]
    fn test_delete_object() {
        let repo = CatalogRepository::open_in_memory().unwrap();

        let obj = CachedObject {
            backend_id: "local-1".to_string(),
            stable_id: "inode-456".to_string(),
            display_path: "file.txt".to_string(),
            size: Some(100),
            mtime: None,
            content_type: None,
            etag: None,
            cached_level: MetadataLevel::L0,
            last_accessed: SystemTime::now(),
        };

        repo.upsert_object(&obj).unwrap();
        assert!(repo.delete_object("local-1", "inode-456").unwrap());
        assert!(repo.get_object("local-1", "inode-456").unwrap().is_none());
    }

    #[test]
    fn test_thumbnail_crud() {
        let repo = CatalogRepository::open_in_memory().unwrap();

        let obj = CachedObject {
            backend_id: "local-1".to_string(),
            stable_id: "inode-789".to_string(),
            display_path: "photo.jpg".to_string(),
            size: Some(5000),
            mtime: None,
            content_type: Some("image/jpeg".to_string()),
            etag: None,
            cached_level: MetadataLevel::L1,
            last_accessed: SystemTime::now(),
        };
        repo.upsert_object(&obj).unwrap();

        let entry = ThumbnailEntry {
            backend_id: "local-1".to_string(),
            stable_id: "inode-789".to_string(),
            dimension: 256,
            format: ImageFormat::Jpeg,
            cache_file_path: PathBuf::from("/cache/thumb/789_256.jpg"),
            byte_size: 32_768,
            generated_at: SystemTime::now(),
            source_size: Some(5_000),
            source_mtime: None,
            source_etag: None,
        };

        repo.upsert_thumbnail(&entry).unwrap();
        let loaded = repo
            .get_thumbnail("local-1", "inode-789", 256)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.format, ImageFormat::Jpeg);
        assert_eq!(
            loaded.cache_file_path,
            PathBuf::from("/cache/thumb/789_256.jpg")
        );
        assert_eq!(loaded.byte_size, 32_768);
        assert_eq!(loaded.source_size, Some(5_000));

        let list = repo.list_thumbnails("local-1", "inode-789").unwrap();
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn test_snapshot_crud() {
        let repo = CatalogRepository::open_in_memory().unwrap();

        let snap = DirectorySnapshot {
            backend_id: "local-1".to_string(),
            dir_stable_id: "inode-100".to_string(),
            generation: 0,
            item_count: 42,
            snapshot_at: SystemTime::now(),
            is_complete: true,
        };

        repo.upsert_snapshot(&snap).unwrap();
        let loaded = repo.get_snapshot("local-1", "inode-100").unwrap().unwrap();
        assert_eq!(loaded.item_count, 42);
        assert!(loaded.is_complete);
    }

    #[test]
    fn snapshot_generation_stays_visible_until_replacement_completes() {
        let mut repo = CatalogRepository::open_in_memory().unwrap();
        let first = CachedDirectoryEntry {
            stable_id: "first-id".to_string(),
            display_path: "/root/first.txt".to_string(),
            name: "first.txt".to_string(),
            is_dir: false,
            metadata_level: MetadataLevel::L1,
            size: Some(10),
            mtime: None,
            ctime: None,
            is_symlink: false,
            symlink_target: None,
            is_inaccessible: false,
        };
        let first_generation = repo.begin_snapshot_generation("local", "root-id").unwrap();
        repo.append_snapshot_entries(
            "local",
            "root-id",
            first_generation,
            std::slice::from_ref(&first),
        )
        .unwrap();
        repo.complete_snapshot_generation("local", "root-id", first_generation)
            .unwrap();
        repo.upsert_snapshot(&DirectorySnapshot {
            backend_id: "local".to_string(),
            dir_stable_id: "root-id".to_string(),
            generation: 0,
            item_count: 1,
            snapshot_at: SystemTime::now(),
            is_complete: true,
        })
        .unwrap();
        assert_eq!(
            repo.get_snapshot("local", "root-id")
                .unwrap()
                .unwrap()
                .generation,
            first_generation
        );

        let second = CachedDirectoryEntry {
            stable_id: "second-id".to_string(),
            display_path: "/root/second".to_string(),
            name: "second".to_string(),
            is_dir: true,
            metadata_level: MetadataLevel::L0,
            size: None,
            mtime: None,
            ctime: None,
            is_symlink: false,
            symlink_target: None,
            is_inaccessible: false,
        };
        let second_generation = repo.begin_snapshot_generation("local", "root-id").unwrap();
        repo.append_snapshot_entries(
            "local",
            "root-id",
            second_generation,
            std::slice::from_ref(&second),
        )
        .unwrap();

        assert_eq!(
            repo.get_snapshot_entries("local", "root-id")
                .unwrap()
                .unwrap(),
            vec![first]
        );

        repo.complete_snapshot_generation("local", "root-id", second_generation)
            .unwrap();
        assert_eq!(
            repo.get_snapshot_entries("local", "root-id")
                .unwrap()
                .unwrap(),
            vec![second]
        );
        assert!(second_generation > first_generation);
    }

    #[test]
    fn snapshot_keeps_distinct_paths_that_share_a_stable_id() {
        let mut repo = CatalogRepository::open_in_memory().unwrap();
        let entry = |name: &str| CachedDirectoryEntry {
            stable_id: "shared-inode".to_string(),
            display_path: format!("/root/{name}"),
            name: name.to_string(),
            is_dir: false,
            metadata_level: MetadataLevel::L0,
            size: None,
            mtime: None,
            ctime: None,
            is_symlink: false,
            symlink_target: None,
            is_inaccessible: false,
        };
        let generation = repo.begin_snapshot_generation("local", "root-id").unwrap();
        repo.append_snapshot_entries(
            "local",
            "root-id",
            generation,
            &[entry("first-link"), entry("second-link")],
        )
        .unwrap();
        let snapshot = repo
            .complete_snapshot_generation("local", "root-id", generation)
            .unwrap();

        assert_eq!(snapshot.item_count, 2);
        assert_eq!(
            repo.get_snapshot_entries("local", "root-id")
                .unwrap()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn test_rebind_display_path_prefix_updates_descendants() {
        let repo = CatalogRepository::open_in_memory().unwrap();
        let now = SystemTime::now();
        let parent = CachedObject {
            backend_id: "local-1".to_string(),
            stable_id: "dir-1".to_string(),
            display_path: "/old".to_string(),
            size: None,
            mtime: None,
            content_type: None,
            etag: None,
            cached_level: MetadataLevel::L1,
            last_accessed: now,
        };
        let child = CachedObject {
            backend_id: "local-1".to_string(),
            stable_id: "file-1".to_string(),
            display_path: "/old/file.jpg".to_string(),
            size: Some(1),
            mtime: None,
            content_type: Some("image/jpeg".to_string()),
            etag: None,
            cached_level: MetadataLevel::L1,
            last_accessed: now,
        };
        repo.upsert_object(&parent).unwrap();
        repo.upsert_object(&child).unwrap();

        let updated = repo
            .rebind_display_path_prefix("local-1", "/old", "/new")
            .unwrap();

        assert_eq!(updated, 2);
        assert_eq!(
            repo.get_object("local-1", "dir-1")
                .unwrap()
                .unwrap()
                .display_path,
            "/new"
        );
        assert_eq!(
            repo.get_object("local-1", "file-1")
                .unwrap()
                .unwrap()
                .display_path,
            "/new/file.jpg"
        );
    }

    #[test]
    fn list_object_display_paths_by_prefix_limits_ordered_matches() {
        let repo = CatalogRepository::open_in_memory().unwrap();
        let now = SystemTime::now();
        for (stable_id, display_path) in [
            ("dir-1", "/photos"),
            ("file-1", "/photos/a.jpg"),
            ("file-2", "/photos/b.jpg"),
            ("file-3", "/photoshoot/c.jpg"),
        ] {
            repo.upsert_object(&CachedObject {
                backend_id: "local-1".to_string(),
                stable_id: stable_id.to_string(),
                display_path: display_path.to_string(),
                size: None,
                mtime: None,
                content_type: None,
                etag: None,
                cached_level: MetadataLevel::L1,
                last_accessed: now,
            })
            .unwrap();
        }

        let matches = repo
            .list_object_display_paths_by_prefix("local-1", "/photos", 3)
            .unwrap();

        assert_eq!(
            matches,
            vec![
                "/photos".to_string(),
                "/photos/a.jpg".to_string(),
                "/photos/b.jpg".to_string()
            ]
        );
    }

    #[test]
    fn test_delete_objects_by_display_path_prefix_removes_thumbnail_files() {
        let repo = CatalogRepository::open_in_memory().unwrap();
        let now = SystemTime::now();
        let obj = CachedObject {
            backend_id: "local-1".to_string(),
            stable_id: "file-2".to_string(),
            display_path: "/gone/file.jpg".to_string(),
            size: Some(10),
            mtime: None,
            content_type: Some("image/jpeg".to_string()),
            etag: None,
            cached_level: MetadataLevel::L1,
            last_accessed: now,
        };
        repo.upsert_object(&obj).unwrap();

        let dir = tempdir().unwrap();
        let thumb_path = dir.path().join("gone-thumb.jpg");
        std::fs::write(&thumb_path, b"thumb").unwrap();
        repo.upsert_thumbnail(&ThumbnailEntry {
            backend_id: "local-1".to_string(),
            stable_id: "file-2".to_string(),
            dimension: 256,
            format: ImageFormat::Jpeg,
            cache_file_path: thumb_path.clone(),
            byte_size: 5,
            generated_at: now,
            source_size: Some(10),
            source_mtime: None,
            source_etag: None,
        })
        .unwrap();

        let removed = repo
            .delete_objects_by_display_path_prefix("local-1", "/gone")
            .unwrap();

        assert_eq!(removed, vec!["file-2".to_string()]);
        assert!(repo.get_object("local-1", "file-2").unwrap().is_none());
        assert!(repo
            .get_thumbnail("local-1", "file-2", 256)
            .unwrap()
            .is_none());
        assert!(!thumb_path.exists());
    }
}
