use crate::common::{
    create_file_for_write, open_file_for_read, rename_no_replace, write_range_to_file,
    write_stream_to_open_file,
};
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
use iris_platform_integration::copy::{platform_copy_blocking, stream_file_copy_blocking};
use iris_platform_integration::path::{ensure_within, numbered_sibling_path};
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

/// Metadata of a local file or directory; the content type is guessed from the name.
pub fn metadata_result(
    path: &Path,
    metadata: &std::fs::Metadata,
    level: MetadataLevel,
) -> MetadataResult {
    MetadataResult {
        level,
        is_dir: metadata.is_dir(),
        size: metadata.is_file().then_some(metadata.len()),
        mtime: metadata.modified().ok(),
        ctime: metadata.created().ok(),
        content_type: mime_guess::from_path(path)
            .first()
            .map(|mime| mime.to_string()),
        etag: None,
    }
}

/// Local file system adapter, also used for network mounts such as NFS.
pub struct LocalFsAdapter {
    root: PathBuf,
    backend_id: BackendId,
    can_clone: bool,
    /// NFS mount: no trash, range writes, reflink or unverified no-replace mutations.
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

    /// NFS mutations with Fail/Skip/Rename fail closed; server-side exclusion is unverified.
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

    fn check_conflict_support(&self, resolution: &ConflictResolution) -> Result<(), BackendError> {
        // A local rename flag does not establish server-side exclusion on arbitrary NFS.
        // Until that protocol guarantee is verified, reject rather than emulate replacement.
        if self.network_mount && !matches!(resolution, ConflictResolution::Overwrite) {
            return Err(BackendError::Unsupported {
                operation: "atomic non-overwrite mutation on NFS share",
            });
        }
        Ok(())
    }

    fn early_conflict(
        dst_path: &Path,
        display_path: &str,
        resolution: &ConflictResolution,
    ) -> Result<(), BackendError> {
        // Inexpensive pre-check so existing Fail/Skip targets never stage payloads.
        // Uses symlink_metadata so directories and dangling links count as occupied.
        // The final no-replace install remains the authority and closes races.
        if matches!(
            resolution,
            ConflictResolution::Fail | ConflictResolution::Skip
        ) && std::fs::symlink_metadata(dst_path).is_ok()
        {
            return Err(conflict_for(display_path));
        }
        Ok(())
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
        Ok(metadata_result(&path, &metadata, MetadataLevel::L1))
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
        let len = data.len() as u64;
        self.write_from_reader(
            dst,
            Box::pin(std::io::Cursor::new(data)),
            Some(len),
            on_conflict,
        )
        .await
    }

    async fn write_from_reader(
        &self,
        dst: &ObjectRef,
        reader: Pin<Box<dyn AsyncRead + Send>>,
        content_length: Option<u64>,
        on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        let dst_path = self.resolve(dst)?;
        self.check_conflict_support(&on_conflict)?;
        Self::early_conflict(&dst_path, &dst.display_path, &on_conflict)?;
        debug!(
            backend_id = %self.backend_id,
            path_hash = %iris_core_domain::observability::path_hash(&dst.display_path),
            content_length = content_length,
            "backend.localfs.write_stream"
        );
        let open_dst = dst_path.clone();
        let open_mode = on_conflict.clone();
        let open_display = dst.display_path.clone();
        // Keep path creation and its owner on the same worker: a cancelled future must not
        // clean staging before a detached filesystem call has finished creating it.
        let (target, file) = tokio::task::spawn_blocking(move || {
            Self::early_conflict(&open_dst, &open_display, &open_mode)?;
            let target = WriteDestination::new(&open_dst, &open_mode)?;
            if let Some(parent) = target.path().parent() {
                std::fs::create_dir_all(parent).map_err(|e| BackendError::Internal(Box::new(e)))?;
            }
            let file = create_file_for_write(target.path(), content_length)
                .map_err(|e| BackendError::Internal(Box::new(e)))?;
            Ok::<_, BackendError>((target, file))
        })
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))??;
        write_stream_to_open_file(file, reader, content_length).await?;
        tokio::task::spawn_blocking(move || target.install(&dst_path, &on_conflict))
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))??;
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

        self.check_conflict_support(&options.on_conflict)?;
        Self::early_conflict(&dst_path, &dst.display_path, &options.on_conflict)?;
        let can_clone = self.can_clone;
        let copy_display = dst.display_path.clone();
        // The worker owns staging until copying and installation finish, even if the caller
        // cancels its future. No detached copy can outlive cleanup and recreate its paths.
        tokio::task::spawn_blocking(move || {
            Self::early_conflict(&dst_path, &copy_display, &options.on_conflict)?;
            let target = WriteDestination::new(&dst_path, &options.on_conflict)?;
            let cloned = can_clone
                && try_reflink_copy(&src_path, target.path())
                    .map_err(|e| BackendError::Internal(Box::new(e)))?;
            if !cloned {
                match platform_copy_blocking(&src_path, target.path()) {
                    Ok(_) => {}
                    Err(error) if error.should_fallback_to_streaming() => {
                        stream_file_copy_blocking(&src_path, target.path())
                            .map_err(|error| BackendError::Internal(Box::new(error)))?;
                    }
                    Err(error) => return Err(BackendError::Internal(Box::new(error))),
                }
            }
            target.install(&dst_path, &options.on_conflict)
        })
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))??;
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

        self.check_conflict_support(&options.on_conflict)?;
        Self::early_conflict(&dst_path, &dst.display_path, &options.on_conflict)?;
        let move_display = dst.display_path.clone();
        tokio::task::spawn_blocking(move || {
            // Re-check on the worker so a creator between the async check and this
            // task still meets the atomic install below as the authority.
            Self::early_conflict(&dst_path, &move_display, &options.on_conflict)?;
            install_with_conflict(&src_path, &dst_path, &options.on_conflict)
        })
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))??;

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

    async fn get_metadata(
        &self,
        object: &ObjectRef,
        level: MetadataLevel,
    ) -> Result<MetadataResult, BackendError> {
        let path = self.resolve(object)?;
        let metadata = fs::metadata(&path)
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;
        // Beyond L1 only the content type is guessed; EXIF and the like come from thumb-preview.
        Ok(metadata_result(&path, &metadata, level))
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

/// Non-overwrite operations build complete data in an exclusively owned sibling directory.
/// Nothing under the destination name is opened before atomic installation. Overwrite retains
/// the existing direct-write/copy behavior. Drop also runs on errors and cancelled futures.
struct WriteDestination {
    path: PathBuf,
    staging_dir: Option<PathBuf>,
}

impl WriteDestination {
    fn new(dst: &Path, resolution: &ConflictResolution) -> Result<Self, BackendError> {
        if matches!(resolution, ConflictResolution::Overwrite) {
            return Ok(Self {
                path: dst.to_path_buf(),
                staging_dir: None,
            });
        }
        let parent = dst.parent().ok_or_else(|| {
            BackendError::Internal(Box::new(std::io::Error::other("destination has no parent")))
        })?;
        let reserve = || -> std::io::Result<Self> {
            std::fs::create_dir_all(parent)?;
            static NEXT_WRITE_ID: AtomicU64 = AtomicU64::new(0);
            loop {
                let id = NEXT_WRITE_ID.fetch_add(1, Ordering::Relaxed);
                let dir = parent.join(format!(".iris-write-{}-{id}", std::process::id()));
                #[cfg(unix)]
                let result = {
                    use std::os::unix::fs::DirBuilderExt;
                    std::fs::DirBuilder::new().mode(0o700).create(&dir)
                };
                #[cfg(not(unix))]
                let result = std::fs::create_dir(&dir);
                match result {
                    Ok(()) => {
                        return Ok(Self {
                            path: dir.join("data"),
                            staging_dir: Some(dir),
                        })
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(e),
                }
            }
        };
        reserve().map_err(|e| BackendError::Internal(Box::new(e)))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn install(self, dst: &Path, resolution: &ConflictResolution) -> Result<(), BackendError> {
        if self.staging_dir.is_some() {
            install_with_conflict(&self.path, dst, resolution)?;
        }
        Ok(())
    }
}

impl Drop for WriteDestination {
    fn drop(&mut self) {
        if let Some(dir) = &self.staging_dir {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_dir(dir);
        }
    }
}

fn install_with_conflict(
    src: &Path,
    dst: &Path,
    resolution: &ConflictResolution,
) -> Result<(), BackendError> {
    install_with_conflict_using(src, dst, resolution, rename_no_replace)
}

fn conflict_for(display_path: &str) -> BackendError {
    BackendError::Conflict {
        object: ObjectRef {
            key: ObjectKey {
                backend_id: BackendId("local".to_string()),
                stable_id: StableId("unknown".to_string()),
            },
            parent_stable_id: None,
            display_path: display_path.to_string(),
        },
    }
}

fn is_occupied(path: &Path) -> bool {
    // symlink_metadata treats files, directories and dangling links as taken.
    std::fs::symlink_metadata(path).is_ok()
}

fn install_with_conflict_using(
    src: &Path,
    dst: &Path,
    resolution: &ConflictResolution,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), BackendError> {
    if matches!(resolution, ConflictResolution::Overwrite) {
        return std::fs::rename(src, dst).map_err(|e| BackendError::Internal(Box::new(e)));
    }
    let mut candidate = dst.to_path_buf();
    let mut suffixes = 1u64..=u64::MAX;
    loop {
        match rename(src, &candidate) {
            Ok(()) => return Ok(()),
            Err(e) => {
                // Normalize every destination-type collision: files, directories and
                // dangling links must classify as Conflict (or retry for Rename).
                // Some filesystems report directory collisions without AlreadyExists,
                // so an existing entry always means occupied here.
                let occupied =
                    e.kind() == std::io::ErrorKind::AlreadyExists || is_occupied(&candidate);
                if !occupied {
                    return Err(BackendError::Internal(Box::new(e)));
                }
                if matches!(resolution, ConflictResolution::Rename) {
                    let index = suffixes.next().ok_or_else(|| {
                        BackendError::Internal(Box::new(std::io::Error::other(
                            "sibling name suffixes exhausted",
                        )))
                    })?;
                    candidate = numbered_sibling_path(dst, index);
                } else {
                    return Err(conflict_for(&candidate.to_string_lossy()));
                }
            }
        }
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

    fn file_ref(backend_id: &BackendId, name: &str) -> ObjectRef {
        ObjectRef {
            display_path: name.into(),
            ..root_ref(backend_id)
        }
    }

    #[tokio::test]
    async fn existing_entries_obey_conflicts_for_every_mutation() {
        for dangling in [false, true] {
            if dangling && !cfg!(unix) {
                continue;
            }
            for mode in [
                ConflictResolution::Fail,
                ConflictResolution::Skip,
                ConflictResolution::Rename,
                ConflictResolution::Overwrite,
            ] {
                // Overwrite's historical symlink-following behavior for writes is unchanged.
                if dangling && matches!(mode, ConflictResolution::Overwrite) {
                    continue;
                }
                for operation in ["write", "stream", "clone-copy", "byte-copy", "move"] {
                    let temp = TempDir::new().expect("tempdir");
                    let backend_id = BackendId("conflict-test".into());
                    let mut adapter = LocalFsAdapter::new(temp.path().into(), backend_id.clone());
                    adapter.can_clone = operation == "clone-copy";
                    let src = file_ref(&backend_id, "src.txt");
                    let dst = file_ref(&backend_id, "dst.txt");
                    std::fs::write(temp.path().join("src.txt"), b"ours").expect("source");
                    let dst_path = temp.path().join("dst.txt");
                    if dangling {
                        #[cfg(unix)]
                        std::os::unix::fs::symlink("missing", &dst_path).expect("dangling link");
                    } else {
                        std::fs::write(&dst_path, b"theirs").expect("destination");
                    }
                    let result = match operation {
                        "write" => {
                            adapter
                                .write_object(&dst, b"ours".to_vec(), mode.clone())
                                .await
                        }
                        "stream" => {
                            adapter
                                .write_from_reader(
                                    &dst,
                                    Box::pin(std::io::Cursor::new(b"ours")),
                                    Some(4),
                                    mode.clone(),
                                )
                                .await
                        }
                        "move" => {
                            adapter
                                .move_object(
                                    &src,
                                    &dst,
                                    MoveOptions {
                                        on_conflict: mode.clone(),
                                    },
                                )
                                .await
                        }
                        _ => {
                            adapter
                                .copy(
                                    &src,
                                    &dst,
                                    CopyOptions {
                                        on_conflict: mode.clone(),
                                    },
                                )
                                .await
                        }
                    };
                    let renamed = matches!(mode, ConflictResolution::Rename);
                    let overwritten = matches!(mode, ConflictResolution::Overwrite);
                    if renamed || overwritten {
                        result.expect("successful mutation");
                        let output = if renamed {
                            temp.path().join("dst (1).txt")
                        } else {
                            dst_path.clone()
                        };
                        assert_eq!(std::fs::read(output).expect("output"), b"ours");
                    } else {
                        assert!(
                            matches!(result, Err(BackendError::Conflict { .. })),
                            "{operation} {mode:?}: {result:?}"
                        );
                    }
                    if !overwritten {
                        if dangling {
                            assert_eq!(
                                std::fs::read_link(&dst_path).expect("link survives"),
                                Path::new("missing")
                            );
                            assert!(!temp.path().join("missing").exists());
                        } else {
                            assert_eq!(std::fs::read(&dst_path).expect("destination"), b"theirs");
                        }
                    }
                    assert_eq!(
                        temp.path().join("src.txt").exists(),
                        operation != "move" || !(renamed || overwritten)
                    );
                    assert!(std::fs::read_dir(temp.path())
                        .expect("list")
                        .all(|entry| !entry
                            .expect("entry")
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".iris-")));
                }
            }
        }
    }

    #[tokio::test]
    async fn directory_entries_obey_conflicts_for_every_mutation() {
        for mode in [
            ConflictResolution::Fail,
            ConflictResolution::Skip,
            ConflictResolution::Rename,
            ConflictResolution::Overwrite,
        ] {
            for operation in ["write", "stream", "clone-copy", "byte-copy", "move"] {
                let temp = TempDir::new().expect("tempdir");
                let backend_id = BackendId("dir-conflict-test".into());
                let mut adapter = LocalFsAdapter::new(temp.path().into(), backend_id.clone());
                adapter.can_clone = operation == "clone-copy";
                let src = file_ref(&backend_id, "src.txt");
                let dst = file_ref(&backend_id, "dst.txt");
                std::fs::write(temp.path().join("src.txt"), b"ours").expect("source");
                let dst_path = temp.path().join("dst.txt");
                std::fs::create_dir(&dst_path).expect("directory destination");
                let result = match operation {
                    "write" => {
                        adapter
                            .write_object(&dst, b"ours".to_vec(), mode.clone())
                            .await
                    }
                    "stream" => {
                        adapter
                            .write_from_reader(
                                &dst,
                                Box::pin(std::io::Cursor::new(b"ours")),
                                Some(4),
                                mode.clone(),
                            )
                            .await
                    }
                    "move" => {
                        adapter
                            .move_object(
                                &src,
                                &dst,
                                MoveOptions {
                                    on_conflict: mode.clone(),
                                },
                            )
                            .await
                    }
                    _ => {
                        adapter
                            .copy(
                                &src,
                                &dst,
                                CopyOptions {
                                    on_conflict: mode.clone(),
                                },
                            )
                            .await
                    }
                };
                if matches!(mode, ConflictResolution::Rename) {
                    result.expect("rename retries past directory");
                    assert!(dst_path.is_dir(), "directory destination survives");
                    assert_eq!(
                        std::fs::read(temp.path().join("dst (1).txt")).expect("renamed output"),
                        b"ours"
                    );
                } else if matches!(mode, ConflictResolution::Overwrite) {
                    assert!(
                        matches!(result, Err(BackendError::Internal(_))),
                        "{operation} overwrite onto directory must not replace it: {result:?}"
                    );
                    assert!(
                        dst_path.is_dir(),
                        "directory destination survives overwrite attempt"
                    );
                } else {
                    assert!(
                        matches!(result, Err(BackendError::Conflict { .. })),
                        "{operation} {mode:?} directory must conflict: {result:?}"
                    );
                    assert!(dst_path.is_dir(), "directory destination survives");
                }
                // A renamed move consumes the source; failed and overwrite-attempted
                // moves keep it.
                let renamed = matches!(mode, ConflictResolution::Rename);
                assert_eq!(
                    temp.path().join("src.txt").exists(),
                    operation != "move" || !renamed,
                    "source lifetime for directory collision"
                );
                assert!(std::fs::read_dir(temp.path())
                    .expect("list")
                    .all(|entry| !entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".iris-")));
            }
        }
    }

    #[tokio::test]
    async fn existing_fail_skip_short_circuits_without_staging_payload() {
        use std::task::{Context, Poll};
        struct CountingReader {
            polls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }
        impl tokio::io::AsyncRead for CountingReader {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                self.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                buf.put_slice(b"ours");
                Poll::Ready(Ok(()))
            }
        }
        for is_dir in [false, true] {
            for dangling in [false, true] {
                if dangling && (is_dir || !cfg!(unix)) {
                    continue;
                }
                for mode in [ConflictResolution::Fail, ConflictResolution::Skip] {
                    let temp = TempDir::new().expect("tempdir");
                    let backend_id = BackendId("short-circuit-test".into());
                    let adapter = LocalFsAdapter::new(temp.path().into(), backend_id.clone());
                    let dst = file_ref(&backend_id, "dst.txt");
                    let dst_path = temp.path().join("dst.txt");
                    if dangling {
                        #[cfg(unix)]
                        std::os::unix::fs::symlink("missing", &dst_path).expect("dangling link");
                    } else if is_dir {
                        std::fs::create_dir(&dst_path).expect("directory");
                    } else {
                        std::fs::write(&dst_path, b"theirs").expect("destination");
                    }
                    let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                    let reader = CountingReader {
                        polls: std::sync::Arc::clone(&polls),
                    };
                    let result = adapter
                        .write_from_reader(&dst, Box::pin(reader), Some(4), mode.clone())
                        .await;
                    assert!(
                        matches!(result, Err(BackendError::Conflict { .. })),
                        "existing target must conflict without staging: {result:?}"
                    );
                    assert_eq!(
                        polls.load(std::sync::atomic::Ordering::SeqCst),
                        0,
                        "payload must not be read when the target already exists"
                    );
                    if dangling {
                        #[cfg(unix)]
                        assert_eq!(
                            std::fs::read_link(&dst_path).expect("link survives"),
                            Path::new("missing")
                        );
                    } else if is_dir {
                        assert!(dst_path.is_dir());
                    } else {
                        assert_eq!(std::fs::read(&dst_path).expect("destination"), b"theirs");
                    }
                    assert!(std::fs::read_dir(temp.path())
                        .expect("list")
                        .all(|entry| !entry
                            .expect("entry")
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".iris-")));
                }
            }
        }
    }

    #[test]
    fn install_handles_creators_after_selection_and_fails_closed() {
        for mode in [
            ConflictResolution::Fail,
            ConflictResolution::Skip,
            ConflictResolution::Rename,
        ] {
            let temp = TempDir::new().expect("tempdir");
            let src = temp.path().join("src.txt");
            let dst = temp.path().join("dst.txt");
            std::fs::write(&src, b"ours").expect("source");
            let mut attempts = 0;
            let result = install_with_conflict_using(&src, &dst, &mode, |src, selected| {
                attempts += 1;
                // Create the original and then the first suffix only AFTER each was selected.
                if attempts <= 2 {
                    std::fs::write(selected, b"racer")?;
                }
                rename_no_replace(src, selected)
            });
            assert_eq!(std::fs::read(&dst).expect("racer survives"), b"racer");
            if matches!(mode, ConflictResolution::Rename) {
                result.expect("retried rename");
                assert_eq!(attempts, 3);
                assert_eq!(
                    std::fs::read(temp.path().join("dst (1).txt")).expect("first racer"),
                    b"racer"
                );
                assert_eq!(
                    std::fs::read(temp.path().join("dst (2).txt")).expect("ours"),
                    b"ours"
                );
            } else {
                assert!(matches!(result, Err(BackendError::Conflict { .. })));
                assert_eq!(attempts, 1);
                assert_eq!(std::fs::read(&src).expect("source survives"), b"ours");
            }
        }
        let temp = TempDir::new().expect("tempdir");
        let src = temp.path().join("source");
        let dst = temp.path().join("destination");
        std::fs::write(&src, b"keep").expect("source");
        let result =
            install_with_conflict_using(&src, &dst, &ConflictResolution::Rename, |_, _| {
                Err(std::io::ErrorKind::Unsupported.into())
            });
        assert!(matches!(result, Err(BackendError::Internal(_))));
        assert!(src.exists());
        assert!(!dst.exists());
    }

    #[tokio::test]
    async fn streamed_write_conflicts_with_a_creator_during_read_and_cleans_failed_data() {
        struct CreatingReader {
            destination: PathBuf,
            read: bool,
        }
        impl AsyncRead for CreatingReader {
            fn poll_read(
                mut self: Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                buffer: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                if !self.read {
                    std::fs::write(&self.destination, b"racer")?;
                    buffer.put_slice(b"ours");
                    self.read = true;
                }
                std::task::Poll::Ready(Ok(()))
            }
        }
        let temp = TempDir::new().expect("tempdir");
        let backend_id = BackendId("stream-race-test".into());
        let adapter = LocalFsAdapter::new(temp.path().into(), backend_id.clone());
        let dst = file_ref(&backend_id, "dst.txt");
        let reader = CreatingReader {
            destination: temp.path().join("dst.txt"),
            read: false,
        };
        let result = adapter
            .write_from_reader(&dst, Box::pin(reader), Some(4), ConflictResolution::Fail)
            .await;
        assert!(matches!(result, Err(BackendError::Conflict { .. })));
        assert_eq!(
            std::fs::read(temp.path().join("dst.txt")).expect("racer"),
            b"racer"
        );
        let short = file_ref(&backend_id, "short.txt");
        assert!(adapter
            .write_from_reader(
                &short,
                Box::pin(std::io::Cursor::new(b"short")),
                Some(100),
                ConflictResolution::Rename
            )
            .await
            .is_err());
        assert!(!temp.path().join("short.txt").exists());
        assert_eq!(std::fs::read_dir(temp.path()).expect("list").count(), 1);
    }

    #[tokio::test]
    async fn nfs_non_overwrite_fails_closed_without_mutation() {
        let temp = TempDir::new().expect("tempdir");
        let backend_id = BackendId("nfs-conflict-test".into());
        let adapter = LocalFsAdapter::nfs_mount(temp.path().into(), backend_id.clone());
        let src = file_ref(&backend_id, "src");
        let dst = file_ref(&backend_id, "dst");
        std::fs::write(temp.path().join("src"), b"source").expect("source");
        for mode in [
            ConflictResolution::Fail,
            ConflictResolution::Skip,
            ConflictResolution::Rename,
        ] {
            assert!(matches!(
                adapter
                    .write_object(&dst, b"new".to_vec(), mode.clone())
                    .await,
                Err(BackendError::Unsupported { .. })
            ));
            assert!(matches!(
                adapter
                    .copy(
                        &src,
                        &dst,
                        CopyOptions {
                            on_conflict: mode.clone()
                        }
                    )
                    .await,
                Err(BackendError::Unsupported { .. })
            ));
            assert!(matches!(
                adapter
                    .move_object(&src, &dst, MoveOptions { on_conflict: mode })
                    .await,
                Err(BackendError::Unsupported { .. })
            ));
        }
        assert_eq!(std::fs::read_dir(temp.path()).expect("list").count(), 1);
        adapter
            .write_object(&dst, b"allowed".to_vec(), ConflictResolution::Overwrite)
            .await
            .expect("overwrite is still available");
        assert_eq!(
            std::fs::read(temp.path().join("dst")).expect("read"),
            b"allowed"
        );
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
