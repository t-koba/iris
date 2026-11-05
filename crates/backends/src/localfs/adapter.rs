use crate::common::{open_file_for_read, write_range_to_file, write_stream_to_file};
use crate::localfs::capabilities::localfs_capabilities;
use crate::localfs::reflink::{platform_may_support_reflink, try_reflink_copy};
use crate::localfs::stable_id::get_stable_id;
use async_trait::async_trait;
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::capability::CapabilitySet;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::list::{EntryNameOnly, ListCursor, ListPage, ListedEntry};
use iris_core_domain::types::metadata::{MetadataLevel, MetadataResult};
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_core_domain::types::operation::{OperationKind, OperationTicket, TicketId};
use iris_core_domain::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use iris_core_domain::types::progress::ByteRange;
use iris_core_domain::types::read_handle::ReadHandle;
use iris_core_domain::types::session::BackendSession;
use iris_core_domain::types::thumbnail::{ThumbnailHint, ThumbnailResult};
use iris_platform_integration::copy::{platform_copy, stream_file_copy};
use iris_platform_integration::path::{ensure_within, unique_sibling_path};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::fs;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;
use tracing::debug;

const LOCAL_LIST_PAGE_SIZE: usize = 100;
const LOCAL_LIST_CURSOR_PREFIX: &str = "localfs:";
const MAX_ACTIVE_LIST_CURSORS: usize = 64;
const LIST_CURSOR_TTL: Duration = Duration::from_secs(30);

/// Local file system adapter, also used for network mounts such as NFS.
pub struct LocalFsAdapter {
    root: PathBuf,
    backend_id: BackendId,
    can_clone: bool,
    /// NFS mount: no trash, range writes or reflink.
    network_mount: bool,
    ticket_counter: AtomicU64,
    list_cursors: Mutex<LocalListCursorRegistry>,
}

struct LocalListCursorState {
    read_dir: fs::ReadDir,
    parent_stable_id: StableId,
    pending_entry: Option<ListedEntry>,
    last_used: Instant,
}

struct LocalListCursorRegistry {
    next_id: u64,
    states: HashMap<u64, LocalListCursorState>,
}

impl Default for LocalListCursorRegistry {
    fn default() -> Self {
        Self {
            next_id: 1,
            states: HashMap::new(),
        }
    }
}

impl LocalListCursorRegistry {
    fn insert(&mut self, mut state: LocalListCursorState, now: Instant) -> ListCursor {
        self.evict_expired(now);
        while self.states.len() >= MAX_ACTIVE_LIST_CURSORS {
            let Some(oldest_id) = self
                .states
                .iter()
                .min_by_key(|(_, state)| state.last_used)
                .map(|(id, _)| *id)
            else {
                break;
            };
            self.states.remove(&oldest_id);
        }

        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        state.last_used = now;
        self.states.insert(id, state);
        ListCursor(format!("{LOCAL_LIST_CURSOR_PREFIX}{id}"))
    }

    fn take(
        &mut self,
        cursor: &ListCursor,
        now: Instant,
    ) -> Result<LocalListCursorState, BackendError> {
        self.evict_expired(now);
        let id = parse_local_list_cursor(cursor)?;
        self.states.remove(&id).ok_or_else(|| {
            invalid_cursor_error(
                &cursor.0,
                "localfs list cursor is expired or already consumed",
            )
        })
    }

    fn evict_expired(&mut self, now: Instant) {
        self.states
            .retain(|_, state| now.saturating_duration_since(state.last_used) <= LIST_CURSOR_TTL);
    }
}

impl LocalFsAdapter {
    pub fn new(root: PathBuf, backend_id: BackendId) -> Self {
        Self {
            root,
            backend_id,
            can_clone: platform_may_support_reflink(),
            network_mount: false,
            ticket_counter: AtomicU64::new(1),
            list_cursors: Mutex::new(LocalListCursorRegistry::default()),
        }
    }

    pub fn nfs_mount(root: PathBuf, backend_id: BackendId) -> Self {
        Self {
            root,
            backend_id,
            can_clone: false,
            network_mount: true,
            ticket_counter: AtomicU64::new(1),
            list_cursors: Mutex::new(LocalListCursorRegistry::default()),
        }
    }

    fn resolve(&self, object: &ObjectRef) -> Result<PathBuf, BackendError> {
        let relative = Path::new(&object.display_path);
        ensure_within(&self.root, relative).map_err(|e| BackendError::Internal(Box::new(e)))
    }

    fn next_ticket(&self, kind: OperationKind) -> OperationTicket {
        let id = self.ticket_counter.fetch_add(1, Ordering::Relaxed);
        OperationTicket {
            id: TicketId(id),
            kind,
        }
    }

    fn make_object_ref(
        &self,
        path: &Path,
        parent_stable_id: Option<StableId>,
    ) -> Result<ObjectRef, BackendError> {
        let display_path = path
            .strip_prefix(&self.root)
            .map_err(|e| {
                BackendError::Internal(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "path '{}' is outside root '{}': {}",
                        path.display(),
                        self.root.display(),
                        e
                    ),
                )))
            })?
            .to_string_lossy()
            .to_string();
        let stable_id = get_stable_id(path)?;
        Ok(ObjectRef {
            key: ObjectKey {
                backend_id: self.backend_id.clone(),
                stable_id,
            },
            parent_stable_id,
            display_path,
        })
    }

    async fn initial_list_cursor_state(
        &self,
        dir: &ObjectRef,
    ) -> Result<LocalListCursorState, BackendError> {
        let dir_path = self.resolve(dir)?;
        let read_dir = fs::read_dir(&dir_path)
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;

        Ok(LocalListCursorState {
            read_dir,
            parent_stable_id: dir.key.stable_id.clone(),
            pending_entry: None,
            last_used: Instant::now(),
        })
    }

    async fn take_list_cursor_state(
        &self,
        cursor: ListCursor,
    ) -> Result<LocalListCursorState, BackendError> {
        let mut registry = self.list_cursors.lock().await;
        registry.take(&cursor, Instant::now())
    }

    async fn collect_list_page(
        &self,
        mut state: LocalListCursorState,
    ) -> Result<(Vec<ListedEntry>, Option<LocalListCursorState>), BackendError> {
        let mut entries = Vec::with_capacity(LOCAL_LIST_PAGE_SIZE);

        if let Some(entry) = state.pending_entry.take() {
            entries.push(entry);
        }

        while entries.len() < LOCAL_LIST_PAGE_SIZE {
            let Some(entry) = self.next_list_entry(&mut state).await? else {
                return Ok((entries, None));
            };
            entries.push(entry);
        }

        state.pending_entry = self.next_list_entry(&mut state).await?;
        if state.pending_entry.is_some() {
            state.last_used = Instant::now();
            Ok((entries, Some(state)))
        } else {
            Ok((entries, None))
        }
    }

    async fn next_list_entry(
        &self,
        state: &mut LocalListCursorState,
    ) -> Result<Option<ListedEntry>, BackendError> {
        loop {
            let Some(entry) = state
                .read_dir
                .next_entry()
                .await
                .map_err(|e| BackendError::Internal(Box::new(e)))?
            else {
                return Ok(None);
            };

            if let Some(entry) = self
                .list_entry_from_dir_entry(entry, &state.parent_stable_id)
                .await
            {
                return Ok(Some(entry));
            }
        }
    }

    async fn list_entry_from_dir_entry(
        &self,
        entry: fs::DirEntry,
        parent_stable_id: &StableId,
    ) -> Option<ListedEntry> {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        let display_path = match path.strip_prefix(&self.root) {
            Ok(rel) => rel.to_string_lossy().to_string(),
            Err(_) => return None,
        };

        let file_type = match entry.file_type().await {
            Ok(file_type) => file_type,
            Err(_) => {
                return Some(ListedEntry::NameOnly(EntryNameOnly {
                    object_ref: ObjectRef {
                        key: ObjectKey {
                            backend_id: self.backend_id.clone(),
                            stable_id: StableId(display_path.clone()),
                        },
                        parent_stable_id: Some(parent_stable_id.clone()),
                        display_path,
                    },
                    name,
                    is_dir: false,
                    is_symlink: false,
                    is_inaccessible: true,
                }));
            }
        };

        let is_symlink = file_type.is_symlink();
        let is_dir = file_type.is_dir();

        Some(ListedEntry::NameOnly(EntryNameOnly {
            object_ref: ObjectRef {
                key: ObjectKey {
                    backend_id: self.backend_id.clone(),
                    stable_id: StableId(display_path.clone()),
                },
                parent_stable_id: Some(parent_stable_id.clone()),
                display_path,
            },
            name,
            is_dir,
            is_symlink,
            is_inaccessible: false,
        }))
    }
}

#[async_trait]
impl BackendAdapter for LocalFsAdapter {
    async fn connect(&self) -> Result<BackendSession, BackendError> {
        if !self.root.exists() {
            return Err(BackendError::NotFound {
                object: ObjectRef {
                    key: ObjectKey {
                        backend_id: self.backend_id.clone(),
                        stable_id: StableId("root".to_string()),
                    },
                    parent_stable_id: None,
                    display_path: self.root.to_string_lossy().to_string(),
                },
            });
        }

        let root = self.root.to_string_lossy();
        Ok(BackendSession {
            backend_id: self.backend_id.clone(),
            display_name: if self.network_mount {
                format!("NFS: {root}")
            } else {
                root.into_owned()
            },
        })
    }

    async fn list_names(
        &self,
        dir: &ObjectRef,
        cursor: Option<ListCursor>,
    ) -> Result<ListPage, BackendError> {
        let state = match cursor {
            Some(cursor) => self.take_list_cursor_state(cursor).await?,
            None => self.initial_list_cursor_state(dir).await?,
        };
        let (entries, next_state) = self.collect_list_page(state).await?;
        let next_cursor = if let Some(next_state) = next_state {
            let mut registry = self.list_cursors.lock().await;
            Some(registry.insert(next_state, Instant::now()))
        } else {
            None
        };

        Ok(ListPage {
            entries,
            next_cursor,
        })
        .inspect(|page| {
            debug!(
                backend_id = %self.backend_id,
                path_hash = %iris_core_domain::observability::path_hash(&dir.display_path),
                entries = page.entries.len(),
                has_next = page.next_cursor.is_some(),
                "backend.localfs.list"
            );
        })
    }

    async fn stat(&self, object: &ObjectRef) -> Result<MetadataResult, BackendError> {
        let path = self.resolve(object)?;
        let metadata = fs::metadata(&path)
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;

        let is_dir = metadata.is_dir();
        let content_type = mime_guess::from_path(&path).first().map(|m| m.to_string());

        Ok(MetadataResult {
            level: MetadataLevel::L1,
            is_dir,
            size: if is_dir { None } else { Some(metadata.len()) },
            mtime: metadata.modified().ok(),
            ctime: metadata.created().ok(),
            content_type,
            etag: None,
        })
    }

    async fn read(
        &self,
        object: &ObjectRef,
        range: Option<ByteRange>,
    ) -> Result<ReadHandle, BackendError> {
        /// Large reads batch sequential I/O from slow devices such as SD cards
        /// and keep reading ahead while the writer waits on the network.
        const READ_BUFFER_SIZE: usize = 1024 * 1024; // 1 MB

        let path = self.resolve(object)?;
        debug!(
            backend_id = %self.backend_id,
            path_hash = %iris_core_domain::observability::path_hash(&object.display_path),
            range_offset = range.as_ref().map(|range| range.offset),
            range_length = range.as_ref().and_then(|range| range.length),
            "backend.localfs.read"
        );
        let mut file = open_file_for_read(&path, range.is_none())?;

        let metadata = file
            .metadata()
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;
        let total_size = metadata.len();

        match &range {
            Some(r) => {
                file.seek(std::io::SeekFrom::Start(r.offset))
                    .await
                    .map_err(|e| BackendError::Internal(Box::new(e)))?;

                let content_length = r.length.unwrap_or(total_size.saturating_sub(r.offset));

                let limited = file.take(content_length);
                let buffered = tokio::io::BufReader::with_capacity(READ_BUFFER_SIZE, limited);

                Ok(ReadHandle {
                    stream: Box::pin(buffered),
                    content_length: Some(content_length),
                    content_range: Some(r.clone()),
                })
            }
            None => {
                let buffered = tokio::io::BufReader::with_capacity(READ_BUFFER_SIZE, file);
                Ok(ReadHandle {
                    stream: Box::pin(buffered),
                    content_length: Some(total_size),
                    content_range: None,
                })
            }
        }
    }

    async fn write_object(
        &self,
        dst: &ObjectRef,
        data: Vec<u8>,
        on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        let dst_path = self.resolve(dst)?;
        let final_dst = handle_conflict(&dst_path, &on_conflict)?;
        debug!(
            backend_id = %self.backend_id,
            path_hash = %iris_core_domain::observability::path_hash(&dst.display_path),
            bytes = data.len(),
            "backend.localfs.write_object"
        );

        if let Some(parent) = final_dst.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| BackendError::Internal(Box::new(e)))?;
        }

        fs::write(&final_dst, data)
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;

        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn write_from_reader(
        &self,
        dst: &ObjectRef,
        reader: Pin<Box<dyn AsyncRead + Send>>,
        _content_length: Option<u64>,
        on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        let dst_path = self.resolve(dst)?;
        let final_dst = handle_conflict(&dst_path, &on_conflict)?;
        debug!(
            backend_id = %self.backend_id,
            path_hash = %iris_core_domain::observability::path_hash(&dst.display_path),
            content_length = _content_length,
            "backend.localfs.write_stream"
        );
        write_stream_to_file(&final_dst, reader, _content_length).await?;
        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn write_range_from_reader(
        &self,
        dst: &ObjectRef,
        reader: Pin<Box<dyn AsyncRead + Send>>,
        range: ByteRange,
        final_size: u64,
    ) -> Result<OperationTicket, BackendError> {
        let dst_path = self.resolve(dst)?;
        debug!(
            backend_id = %self.backend_id,
            path_hash = %iris_core_domain::observability::path_hash(&dst.display_path),
            range_offset = range.offset,
            range_length = range.length,
            final_size,
            "backend.localfs.write_range"
        );
        write_range_to_file(&dst_path, reader, range, final_size).await?;
        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn mkdir(&self, parent: &ObjectRef, name: &str) -> Result<ObjectRef, BackendError> {
        let parent_path = self.resolve(parent)?;
        let new_dir = parent_path.join(name);
        debug!(
            backend_id = %self.backend_id,
            parent_path_hash = %iris_core_domain::observability::path_hash(&parent.display_path),
            name_hash = %iris_core_domain::observability::path_hash(name),
            "backend.localfs.mkdir"
        );
        fs::create_dir(&new_dir)
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;

        self.make_object_ref(&new_dir, Some(parent.key.stable_id.clone()))
    }

    async fn set_mtime(
        &self,
        object: &ObjectRef,
        mtime: std::time::SystemTime,
    ) -> Result<(), BackendError> {
        let path = self.resolve(object)?;
        tokio::task::spawn_blocking(move || {
            iris_platform_integration::file_times::preserve_modified_time(&path, mtime)
        })
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))?
        .map(|_| ())
        .map_err(|e| BackendError::Internal(Box::new(e)))
    }

    async fn copy(
        &self,
        src: &ObjectRef,
        dst: &ObjectRef,
        options: CopyOptions,
    ) -> Result<OperationTicket, BackendError> {
        let src_path = self.resolve(src)?;
        let dst_path = self.resolve(dst)?;
        debug!(
            backend_id = %self.backend_id,
            src_path_hash = %iris_core_domain::observability::path_hash(&src.display_path),
            dst_path_hash = %iris_core_domain::observability::path_hash(&dst.display_path),
            "backend.localfs.copy"
        );

        let final_dst = handle_conflict(&dst_path, &options.on_conflict)?;

        if self.can_clone {
            let (clone_src, clone_dst) = (src_path.clone(), final_dst.clone());
            let cloned =
                tokio::task::spawn_blocking(move || try_reflink_copy(&clone_src, &clone_dst))
                    .await
                    .map_err(|e| BackendError::Internal(Box::new(e)))?
                    .map_err(|e| BackendError::Internal(Box::new(e)))?;
            if cloned {
                return Ok(self.next_ticket(OperationKind::Copy));
            }
        }

        match platform_copy(&src_path, &final_dst).await {
            Ok(_) => {}
            Err(error) if error.should_fallback_to_streaming() => {
                stream_file_copy(&src_path, &final_dst)
                    .await
                    .map_err(|error| BackendError::Internal(Box::new(error)))?;
            }
            Err(error) => return Err(BackendError::Internal(Box::new(error))),
        }

        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn move_object(
        &self,
        src: &ObjectRef,
        dst: &ObjectRef,
        options: MoveOptions,
    ) -> Result<OperationTicket, BackendError> {
        let src_path = self.resolve(src)?;
        let dst_path = self.resolve(dst)?;
        debug!(
            backend_id = %self.backend_id,
            src_path_hash = %iris_core_domain::observability::path_hash(&src.display_path),
            dst_path_hash = %iris_core_domain::observability::path_hash(&dst.display_path),
            "backend.localfs.move"
        );

        let final_dst = handle_conflict(&dst_path, &options.on_conflict)?;

        fs::rename(&src_path, &final_dst)
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;

        Ok(self.next_ticket(OperationKind::Move))
    }

    async fn delete(
        &self,
        object: &ObjectRef,
        mode: DeleteMode,
    ) -> Result<OperationTicket, BackendError> {
        let path = self.resolve(object)?;
        debug!(
            backend_id = %self.backend_id,
            path_hash = %iris_core_domain::observability::path_hash(&object.display_path),
            mode = ?mode,
            "backend.localfs.delete"
        );

        match mode {
            DeleteMode::Trash if self.network_mount => {
                return Err(BackendError::Unsupported {
                    operation: "trash on NFS share",
                });
            }
            DeleteMode::Trash => {
                iris_platform_integration::trash::trash_file(&path)
                    .map_err(|e| BackendError::Internal(Box::new(e)))?;
            }
            DeleteMode::Permanent => {
                // `symlink_metadata`: a link (even a dangling one) is removed itself, never the
                // directory it points to.
                let metadata = fs::symlink_metadata(&path)
                    .await
                    .map_err(|e| BackendError::Internal(Box::new(e)))?;
                let result = if metadata.is_dir() {
                    fs::remove_dir_all(&path).await
                } else {
                    match fs::remove_file(&path).await {
                        // Windows removes directory links and junctions with `remove_dir`.
                        Err(error) if cfg!(windows) && metadata.file_type().is_symlink() => {
                            fs::remove_dir(&path).await.map_err(|_| error)
                        }
                        result => result,
                    }
                };
                result.map_err(|e| BackendError::Internal(Box::new(e)))?;
            }
        }

        Ok(self.next_ticket(OperationKind::Delete))
    }

    async fn get_thumbnail(
        &self,
        _object: &ObjectRef,
        _hint: ThumbnailHint,
    ) -> Result<ThumbnailResult, BackendError> {
        // Thumbnails are produced by thumb-preview; local files have no direct thumbnail API.
        Err(BackendError::Unsupported {
            operation: "get_thumbnail",
        })
    }

    async fn get_metadata(
        &self,
        object: &ObjectRef,
        level: MetadataLevel,
    ) -> Result<MetadataResult, BackendError> {
        let path = self.resolve(object)?;
        let metadata = fs::metadata(&path)
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;

        let is_dir = metadata.is_dir();
        let content_type = mime_guess::from_path(&path).first().map(|m| m.to_string());

        // Beyond L1 only the content type is guessed; EXIF and the like come from thumb-preview.
        Ok(MetadataResult {
            level,
            is_dir,
            size: if is_dir { None } else { Some(metadata.len()) },
            mtime: metadata.modified().ok(),
            ctime: metadata.created().ok(),
            content_type,
            etag: None,
        })
    }

    async fn get_stable_id(&self, object: &ObjectRef) -> Result<StableId, BackendError> {
        let path = self.resolve(object)?;
        get_stable_id(&path)
    }

    fn capabilities(&self) -> CapabilitySet {
        let capabilities = localfs_capabilities(self.can_clone);
        if self.network_mount {
            CapabilitySet {
                can_range_write: false,
                can_trash: false,
                ..capabilities
            }
        } else {
            capabilities
        }
    }
}

fn parse_local_list_cursor(cursor: &ListCursor) -> Result<u64, BackendError> {
    let id = cursor
        .0
        .strip_prefix(LOCAL_LIST_CURSOR_PREFIX)
        .ok_or_else(|| invalid_cursor_error(&cursor.0, "missing localfs cursor prefix"))?;
    id.parse::<u64>()
        .map_err(|_| invalid_cursor_error(&cursor.0, "cursor id is not numeric"))
}

fn invalid_cursor_error(cursor: &str, reason: &str) -> BackendError {
    BackendError::Internal(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("invalid list cursor '{cursor}': {reason}"),
    )))
}

fn handle_conflict(
    dst_path: &Path,
    resolution: &ConflictResolution,
) -> Result<PathBuf, BackendError> {
    if !dst_path.exists() {
        return Ok(dst_path.to_path_buf());
    }

    match resolution {
        ConflictResolution::Fail | ConflictResolution::Skip => Err(BackendError::Conflict {
            object: ObjectRef {
                key: ObjectKey {
                    backend_id: BackendId("local".to_string()),
                    stable_id: StableId("unknown".to_string()),
                },
                parent_stable_id: None,
                display_path: dst_path.to_string_lossy().to_string(),
            },
        }),
        ConflictResolution::Rename => Ok(unique_sibling_path(dst_path)),
        ConflictResolution::Overwrite => Ok(dst_path.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn root_ref(backend_id: &BackendId) -> ObjectRef {
        ObjectRef {
            key: ObjectKey {
                backend_id: backend_id.clone(),
                stable_id: StableId("root".to_string()),
            },
            parent_stable_id: None,
            display_path: String::new(),
        }
    }

    async fn create_flat_files(root: &Path, count: usize) {
        for index in 0..count {
            let path = root.join(format!("file-{index:04}.txt"));
            fs::write(path, b"x").await.expect("write synthetic file");
        }
    }

    #[tokio::test]
    async fn list_uses_localfs_cursor_between_pages() {
        let temp_dir = TempDir::new().expect("temp dir");
        create_flat_files(temp_dir.path(), LOCAL_LIST_PAGE_SIZE + 7).await;
        let backend_id = BackendId("localfs-cursor-test".to_string());
        let adapter = LocalFsAdapter::new(temp_dir.path().to_path_buf(), backend_id.clone());
        let root = root_ref(&backend_id);

        let first_page = adapter.list_names(&root, None).await.expect("first page");
        assert_eq!(first_page.entries.len(), LOCAL_LIST_PAGE_SIZE);
        let cursor = first_page.next_cursor.expect("next cursor");
        assert!(cursor.0.starts_with(LOCAL_LIST_CURSOR_PREFIX));

        let second_page = adapter
            .list_names(&root, Some(cursor))
            .await
            .expect("second page");
        assert_eq!(second_page.entries.len(), 7);
        assert!(second_page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn abandoned_list_cursors_are_bounded() {
        let temp_dir = TempDir::new().expect("temp dir");
        create_flat_files(temp_dir.path(), LOCAL_LIST_PAGE_SIZE + 1).await;
        let backend_id = BackendId("localfs-cursor-bound-test".to_string());
        let adapter = LocalFsAdapter::new(temp_dir.path().to_path_buf(), backend_id.clone());
        let root = root_ref(&backend_id);

        for _ in 0..(MAX_ACTIVE_LIST_CURSORS + 8) {
            let page = adapter.list_names(&root, None).await.expect("first page");
            assert!(page.next_cursor.is_some());
        }

        let registry = adapter.list_cursors.lock().await;
        assert!(registry.states.len() <= MAX_ACTIVE_LIST_CURSORS);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn list_leaves_symlink_directory_kind_for_stat_hydration() {
        let temp_dir = TempDir::new().expect("temp dir");
        fs::create_dir(temp_dir.path().join("target-dir"))
            .await
            .expect("create target dir");
        std::os::unix::fs::symlink("target-dir", temp_dir.path().join("link-to-dir"))
            .expect("create symlink");

        let backend_id = BackendId("localfs-symlink-kind-test".to_string());
        let adapter = LocalFsAdapter::new(temp_dir.path().to_path_buf(), backend_id.clone());
        let root = root_ref(&backend_id);

        let page = adapter.list_names(&root, None).await.expect("list root");
        let link_entry = page
            .entries
            .iter()
            .find(|entry| entry.name().name == "link-to-dir")
            .expect("symlink entry");

        assert!(link_entry.name().is_symlink);
        assert!(!link_entry.name().is_dir);
        assert_eq!(link_entry.metadata_level(), MetadataLevel::L0);
        assert!(
            iris_core_domain::types::list::ListEntry::from_listed_entry(link_entry.clone())
                .metadata_pending()
        );

        let metadata = adapter
            .stat(&link_entry.name().object_ref)
            .await
            .expect("stat symlink");
        assert!(metadata.is_dir);
        assert_eq!(metadata.size, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn permanent_delete_removes_links_but_never_their_targets() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let target_dir = temp_dir.path().join("target-dir");
        std::fs::create_dir(&target_dir).expect("create target dir");
        std::fs::write(target_dir.join("keep.txt"), b"keep").expect("write kept file");
        std::os::unix::fs::symlink("target-dir", temp_dir.path().join("dir-link"))
            .expect("dir link");
        std::os::unix::fs::symlink("missing", temp_dir.path().join("dangling"))
            .expect("dangling link");

        let backend_id = BackendId("localfs-delete-link-test".to_string());
        let adapter = LocalFsAdapter::new(temp_dir.path().to_path_buf(), backend_id.clone());
        for name in ["dir-link", "dangling"] {
            let object = ObjectRef {
                key: ObjectKey {
                    backend_id: backend_id.clone(),
                    stable_id: StableId(name.to_string()),
                },
                parent_stable_id: None,
                display_path: name.to_string(),
            };
            adapter
                .delete(&object, DeleteMode::Permanent)
                .await
                .unwrap_or_else(|error| panic!("delete {name}: {error}"));
            assert!(std::fs::symlink_metadata(temp_dir.path().join(name)).is_err());
        }

        assert!(target_dir.join("keep.txt").exists());
    }
}
