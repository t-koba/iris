use crate::smb::capabilities::smb_capabilities;
use crate::smb::connection::{SmbConnectionConfig, SmbListingConfig, SmbNativeSession, SmbPool};
use async_trait::async_trait;
use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::capability::CapabilitySet;
use iris_core_domain::types::error::{BackendError, NetworkErrorDetail};
use iris_core_domain::types::list::{EntryNameOnly, ListCursor, ListPage, ListedEntry};
use iris_core_domain::types::metadata::{MetadataLevel, MetadataResult};
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_core_domain::types::operation::{OperationKind, OperationTicket, TicketId};
use iris_core_domain::types::options::{ConflictResolution, CopyOptions, DeleteMode, MoveOptions};
use iris_core_domain::types::progress::ByteRange;
use iris_core_domain::types::read_handle::ReadHandle;
use iris_core_domain::types::session::BackendSession;
use iris_smb_client::{
    close, copy_file_chunks, create_directory, delete, list_directory_once, open_file_and_read,
    open_file_for_read, open_file_for_write, read_file_ranges_pipelined, rename, set_basic_info,
    stat_path, write_file_at, write_file_chunks_pipelined, Smb2CreateDisposition,
    Smb2FileAttributes, Smb2FileBasicInformation, SmbConnectionActor, SmbConnectionError,
    SmbDirectoryEntry, SmbDirectoryEnumeration, SmbHandle, SmbReadRange, DEFAULT_READ_CHUNK_LENGTH,
    DEFAULT_WRITE_CHUNK_LENGTH,
};
use ring::rand::{SecureRandom, SystemRandom};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

/// SMB adapter built on the native SMB2/3 client (never an OS mount).
/// Prefers server-side copy over client relay and avoids bursts of small random reads.
pub struct SmbAdapter {
    backend_id: BackendId,
    pool: Arc<SmbPool>,
    ticket_counter: AtomicU64,
    listing: SmbListingConfig,
    list_cursors: Mutex<HashMap<String, NativeListCursor>>,
}

struct NativeListCursor {
    directory: ObjectRef,
    generation: u64,
    enumeration: SmbDirectoryEnumeration,
    touched: Instant,
}

async fn close_list_cursors(
    cursors: Vec<NativeListCursor>,
    active_generation: u64,
) -> Result<(), BackendError> {
    let mut active_generation_error = None;
    for mut cursor in cursors {
        if let Err(error) = cursor.enumeration.close().await {
            if cursor.generation == active_generation && active_generation_error.is_none() {
                active_generation_error = Some(BackendError::Network(NetworkErrorDetail {
                    message: format!("SMB native directory cursor cleanup failed: {error}"),
                    is_timeout: false,
                }));
            }
        }
    }
    active_generation_error.map_or(Ok(()), Err)
}

fn invalid_list_cursor() -> BackendError {
    BackendError::Internal(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "SMB list cursor is expired, consumed, or belongs to another directory/generation",
    )))
}

// Armed before OPEN: cancellation can lose even the CREATE response. Never
// asynchronously CLOSE on cancellation: pipelined writes may still own the actor.
struct StreamedWriteGuard {
    pool: Arc<SmbPool>,
    generation: u64,
    armed: bool,
}

impl Drop for StreamedWriteGuard {
    fn drop(&mut self) {
        if self.armed {
            self.pool.mark_invalid(self.generation);
        }
    }
}

impl SmbAdapter {
    pub fn new(config: SmbConnectionConfig, backend_id: BackendId) -> Self {
        Self {
            listing: config.listing.clone(),
            list_cursors: Mutex::new(HashMap::new()),
            pool: Arc::new(SmbPool::new(config)),
            backend_id,
            ticket_counter: AtomicU64::new(1),
        }
    }

    fn next_ticket(&self, kind: OperationKind) -> OperationTicket {
        let id = self.ticket_counter.fetch_add(1, Ordering::Relaxed);
        OperationTicket {
            id: TicketId(id),
            kind,
        }
    }

    async fn native_session(&self) -> Result<SmbNativeSession, BackendError> {
        Ok(self.pool.session().await?.native)
    }

    async fn with_read_reconnect<T, Operation, OperationFuture>(
        &self,
        operation: Operation,
    ) -> Result<T, BackendError>
    where
        Operation: Fn(SmbNativeSession) -> OperationFuture,
        OperationFuture: Future<Output = Result<T, BackendError>>,
    {
        let native = self.native_session().await?;
        match operation(native.clone()).await {
            Err(BackendError::Network(_)) => {
                self.pool.invalidate(native.generation).await;
                let retry_native = self.native_session().await?;
                let result = operation(retry_native.clone()).await;
                if matches!(result, Err(BackendError::Network(_))) {
                    self.pool.invalidate(retry_native.generation).await;
                }
                result
            }
            result => result,
        }
    }

    async fn finish_mutation<T>(
        &self,
        native: &SmbNativeSession,
        result: Result<T, BackendError>,
    ) -> Result<T, BackendError> {
        if matches!(&result, Err(BackendError::Network(_))) {
            self.pool.invalidate(native.generation).await;
        }
        result
    }

    async fn list_names_native(
        &self,
        native: &SmbNativeSession,
        dir: &ObjectRef,
        cursor: Option<ListCursor>,
    ) -> Result<ListPage, BackendError> {
        // Take ownership before any await. Cancellation drops these states and
        // synchronously shuts down their actors; no lock spans SMB I/O.
        let (state, expired) = {
            let mut registry = self.list_cursors.lock().unwrap();
            let now = Instant::now();
            let keys = registry
                .iter()
                .filter(|(_, state)| {
                    now.duration_since(state.touched) >= self.listing.idle_timeout
                        || state.generation < native.generation
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            let expired = keys
                .into_iter()
                .filter_map(|key| registry.remove(&key))
                .collect();
            let state = cursor
                .as_ref()
                .and_then(|cursor| registry.remove(&cursor.0));
            (state, expired)
        };
        close_list_cursors(expired, native.generation).await?;
        let mut state = match (cursor, state) {
            (Some(_), Some(mut state)) => {
                if state.directory.key != dir.key
                    || state.directory.display_path != dir.display_path
                    || state.generation != native.generation
                {
                    if let Err(error) = state.enumeration.close().await {
                        return Err(BackendError::Network(NetworkErrorDetail {
                            message: format!("SMB native directory cursor cleanup failed: {error}"),
                            is_timeout: false,
                        }));
                    }
                    return Err(invalid_list_cursor());
                }
                state
            }
            (Some(_), None) => return Err(invalid_list_cursor()),
            (None, _) => NativeListCursor {
                directory: dir.clone(),
                generation: native.generation,
                enumeration: SmbDirectoryEnumeration::open(
                    &native.actor,
                    smb_relative_path(&dir.display_path)?,
                )
                .await
                .map_err(|error| smb_network_error("open directory", error))?,
                touched: Instant::now(),
            },
        };
        let mut entries = Vec::new();
        while entries.len() < self.listing.page_size.get() && !state.enumeration.is_closed() {
            let remaining =
                std::num::NonZeroUsize::new(self.listing.page_size.get() - entries.len()).unwrap();
            let page = state
                .enumeration
                .next_page(remaining)
                .await
                .map_err(|error| smb_network_error("list directory", error))?;
            entries.extend(
                page.into_iter()
                    .filter(|entry| entry.file_name != "." && entry.file_name != "..")
                    .map(|entry| self.native_listed_entry(dir, entry)),
            );
        }
        let next_cursor = if state.enumeration.is_closed() {
            None
        } else {
            // Random one-use tokens are adapter-owned, never SMB file_index or
            // a numeric offset. A new token is issued for every returned page.
            let mut random = [0u8; 16];
            if SystemRandom::new().fill(&mut random).is_err() {
                let _ = state.enumeration.close().await;
                return Err(BackendError::Internal(Box::new(std::io::Error::other(
                    "SMB cursor randomness unavailable",
                ))));
            }
            let token = format!(
                "smb-list:{}",
                random
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            state.touched = Instant::now();
            let evicted = {
                let mut registry = self.list_cursors.lock().unwrap();
                let mut evicted = Vec::new();
                if registry.len() >= self.listing.max_active.get() {
                    if let Some(key) = registry
                        .iter()
                        .min_by_key(|(_, state)| state.touched)
                        .map(|(key, _)| key.clone())
                    {
                        evicted.extend(registry.remove(&key));
                    }
                }
                evicted.extend(registry.insert(token.clone(), state));
                evicted
            };
            if let Err(error) = close_list_cursors(evicted, native.generation).await {
                // The token was not exposed to the caller; remove its retained
                // handle rather than returning a cursor on a stopped generation.
                drop(self.list_cursors.lock().unwrap().remove(&token));
                return Err(error);
            }
            Some(ListCursor(token))
        };
        Ok(ListPage {
            entries,
            next_cursor,
        })
    }

    fn native_listed_entry(&self, dir: &ObjectRef, entry: SmbDirectoryEntry) -> ListedEntry {
        let display_path = child_display_path(&dir.display_path, &entry.file_name);
        ListedEntry::NameOnly(EntryNameOnly {
            object_ref: ObjectRef {
                key: ObjectKey {
                    backend_id: self.backend_id.clone(),
                    stable_id: StableId(display_path.clone()),
                },
                parent_stable_id: Some(dir.key.stable_id.clone()),
                display_path,
            },
            name: entry.file_name,
            is_dir: entry
                .file_attributes
                .contains(Smb2FileAttributes::DIRECTORY),
            is_symlink: false,
            is_inaccessible: false,
        })
    }

    async fn stat_native(
        &self,
        native: &SmbNativeSession,
        object: &ObjectRef,
        level: MetadataLevel,
    ) -> Result<MetadataResult, BackendError> {
        if object.display_path.is_empty() {
            return Ok(MetadataResult {
                level,
                is_dir: true,
                size: None,
                mtime: None,
                ctime: None,
                content_type: None,
                etag: None,
            });
        }

        let entry = stat_path(&native.actor, smb_relative_path(&object.display_path)?)
            .await
            .map_err(|error| match error {
                SmbConnectionError::UnexpectedStatus {
                    status: 0xc000_000f | 0xc000_0034 | 0xc000_003a,
                    ..
                } => BackendError::NotFound {
                    object: object.clone(),
                },
                error => smb_network_error("stat child", error),
            })?;
        let is_dir = entry
            .file_attributes
            .contains(Smb2FileAttributes::DIRECTORY);

        Ok(MetadataResult {
            level,
            is_dir,
            size: if is_dir {
                None
            } else {
                Some(entry.end_of_file)
            },
            mtime: filetime_to_system_time(entry.last_write_time),
            ctime: filetime_to_system_time(entry.creation_time),
            content_type: mime_guess::from_path(&object.display_path)
                .first()
                .map(|mime| mime.to_string()),
            etag: None,
        })
    }

    async fn read_native(
        &self,
        native: &SmbNativeSession,
        object: &ObjectRef,
        range: Option<ByteRange>,
    ) -> Result<ReadHandle, BackendError> {
        let smb_path = smb_relative_path(&object.display_path)?;
        let start = range.as_ref().map_or(0, |range| range.offset);
        let initial_length = range
            .as_ref()
            .and_then(|range| range.length)
            .unwrap_or(u64::from(DEFAULT_READ_CHUNK_LENGTH))
            .min(u64::from(DEFAULT_READ_CHUNK_LENGTH)) as u32;
        let (file, mut initial_data) = if initial_length == 0 {
            (
                open_file_for_read(&native.actor, smb_path)
                    .await
                    .map_err(|error| smb_network_error("open file", error))?,
                Vec::new(),
            )
        } else {
            open_file_and_read(
                &native.actor,
                smb_path,
                SmbReadRange {
                    offset: start,
                    length: initial_length,
                },
            )
            .await
            .map_err(|error| smb_network_error("open file and read", error))?
        };
        let total_size = file.end_of_file;
        let content_length = range
            .as_ref()
            .and_then(|range| range.length)
            .unwrap_or_else(|| total_size.saturating_sub(start))
            .min(total_size.saturating_sub(start));
        initial_data.truncate(content_length as usize);

        Ok(ReadHandle {
            stream: Box::pin(NativeSmbReadStream::new(
                native.actor.clone(),
                file,
                start,
                content_length,
                initial_data,
                Arc::clone(&self.pool),
                native.generation,
            )),
            content_length: Some(content_length),
            content_range: range,
        })
    }

    async fn native_conflict_display_path(
        &self,
        native: &SmbNativeSession,
        dst: &ObjectRef,
        on_conflict: &ConflictResolution,
    ) -> Result<String, BackendError> {
        if !self.native_path_exists(native, &dst.display_path).await? {
            return Ok(dst.display_path.clone());
        }

        match on_conflict {
            ConflictResolution::Overwrite => Ok(dst.display_path.clone()),
            ConflictResolution::Fail | ConflictResolution::Skip => Err(BackendError::Conflict {
                object: dst.clone(),
            }),
            ConflictResolution::Rename => {
                for attempt in 1..=u32::MAX {
                    let candidate = native_conflict_name(&dst.display_path, attempt)?;
                    if !self.native_path_exists(native, &candidate).await? {
                        return Ok(candidate);
                    }
                }
                Err(BackendError::Internal(Box::new(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "exhausted SMB conflict rename candidates",
                ))))
            }
        }
    }

    async fn native_path_exists(
        &self,
        native: &SmbNativeSession,
        display_path: &str,
    ) -> Result<bool, BackendError> {
        if display_path.is_empty() {
            return Ok(true);
        }
        match self
            .stat_native(
                native,
                &self.object_ref_for_display_path(display_path),
                MetadataLevel::L0,
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(BackendError::NotFound { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn object_ref_for_display_path(&self, display_path: &str) -> ObjectRef {
        ObjectRef {
            key: ObjectKey {
                backend_id: self.backend_id.clone(),
                stable_id: StableId(display_path.to_string()),
            },
            parent_stable_id: parent_stable_id_for_display_path(display_path),
            display_path: display_path.to_string(),
        }
    }

    async fn write_bytes_native(
        &self,
        native: &SmbNativeSession,
        display_path: &str,
        data: Vec<u8>,
        create_disposition: Smb2CreateDisposition,
    ) -> Result<(), BackendError> {
        let smb_path = smb_relative_path(display_path)?;
        let file = open_file_for_write(&native.actor, smb_path, create_disposition)
            .await
            .map_err(|error| smb_network_error("open file for write", error))?;
        let write_result = if data.is_empty() {
            Ok(())
        } else {
            write_file_at(&native.actor, &file, 0, data)
                .await
                .map(|_| ())
                .map_err(|error| smb_network_error("write file", error))
        };
        let close_result = close(&native.actor, file)
            .await
            .map(|_| ())
            .map_err(|error| smb_network_error("close written file", error));
        write_result.and(close_result)
    }

    async fn write_reader_native(
        &self,
        native: &SmbNativeSession,
        display_path: &str,
        mut reader: Pin<Box<dyn AsyncRead + Send>>,
        create_disposition: Smb2CreateDisposition,
    ) -> Result<(), BackendError> {
        let smb_path = smb_relative_path(display_path)?;
        let mut guard = StreamedWriteGuard {
            pool: Arc::clone(&self.pool),
            generation: native.generation,
            armed: true,
        };
        let file = open_file_for_write(&native.actor, smb_path, create_disposition)
            .await
            .map_err(|error| smb_network_error("open file for streaming write", error))?;
        const PIPELINE_DEPTH: usize = 8;
        let mut offset = 0u64;
        let mut buffer = vec![0; DEFAULT_WRITE_CHUNK_LENGTH];
        let mut write_result = Ok(());

        'stream: loop {
            let mut chunks = Vec::with_capacity(PIPELINE_DEPTH);
            for _ in 0..PIPELINE_DEPTH {
                let read = match reader.read(&mut buffer).await {
                    Ok(read) => read,
                    Err(error) => {
                        write_result = Err(BackendError::Internal(Box::new(error)));
                        break 'stream;
                    }
                };
                if read == 0 {
                    break;
                }
                chunks.push((offset, buffer[..read].to_vec()));
                offset = offset.saturating_add(read as u64);
            }
            if chunks.is_empty() {
                break;
            }
            if let Err(error) =
                write_file_chunks_pipelined(&native.actor, &file, chunks, PIPELINE_DEPTH).await
            {
                // The pipeline returns on its first error and detaches sibling
                // tasks (at most PIPELINE_DEPTH). They retain their actor request
                // deadlines. Skip CLOSE: it could race those writes on the wire.
                // The armed guard invalidates and evicts this generation; the
                // actor drains dispatched requests before closing the connection.
                return Err(smb_network_error("streaming write", error));
            }
        }

        let close_result = close(&native.actor, file)
            .await
            .map(|_| ())
            .map_err(|error| smb_network_error("close streamed file", error));
        guard.armed = close_result.is_err();
        // A reader error takes precedence over a cleanup error.
        write_result.and(close_result)
    }

    async fn move_native(
        &self,
        native: &SmbNativeSession,
        src: &ObjectRef,
        dst: &ObjectRef,
        options: &MoveOptions,
    ) -> Result<(), BackendError> {
        let src_metadata = self.stat_native(native, src, MetadataLevel::L0).await?;
        let final_display_path = self
            .native_conflict_display_path(native, dst, &options.on_conflict)
            .await?;
        let source_path = smb_relative_path(&src.display_path)?;
        let target_path = smb_relative_path(&final_display_path)?;
        let replace_if_exists = matches!(options.on_conflict, ConflictResolution::Overwrite);
        rename(
            &native.actor,
            source_path,
            target_path,
            src_metadata.is_dir,
            replace_if_exists,
        )
        .await
        .map_err(|error| smb_network_error("rename", error))
    }

    async fn copy_native(
        &self,
        native: &SmbNativeSession,
        src: &ObjectRef,
        dst: &ObjectRef,
        options: &CopyOptions,
    ) -> Result<(), BackendError> {
        let src_metadata = self.stat_native(native, src, MetadataLevel::L1).await?;
        if src_metadata.is_dir {
            return Err(BackendError::Unsupported {
                operation: "copy SMB native directory with COPYCHUNK",
            });
        }

        let final_display_path = self
            .native_conflict_display_path(native, dst, &options.on_conflict)
            .await?;
        let source_path = smb_relative_path(&src.display_path)?;
        let target_path = smb_relative_path(&final_display_path)?;
        copy_file_chunks(
            &native.actor,
            source_path,
            target_path.clone(),
            native_write_disposition(&options.on_conflict),
        )
        .await
        .map_err(|error| smb_network_error("server-side copy", error))?;

        if let Some(mtime) = src_metadata.mtime {
            set_basic_info(
                &native.actor,
                target_path,
                Smb2FileBasicInformation {
                    creation_time: 0,
                    last_access_time: 0,
                    last_write_time: system_time_to_filetime(mtime)?,
                    change_time: 0,
                    file_attributes: Smb2FileAttributes(0),
                },
            )
            .await
            .map_err(|error| smb_network_error("set copied file times", error))?;
        }

        Ok(())
    }

    async fn delete_native_recursive(
        &self,
        native: &SmbNativeSession,
        object: &ObjectRef,
    ) -> Result<(), BackendError> {
        if object.display_path.is_empty() {
            return Err(BackendError::Unsupported {
                operation: "delete SMB share root",
            });
        }

        let mut stack = vec![(object.display_path.clone(), false)];
        while let Some((display_path, visited)) = stack.pop() {
            let object = self.object_ref_for_display_path(&display_path);
            let metadata = self.stat_native(native, &object, MetadataLevel::L0).await?;
            let smb_path = smb_relative_path(&display_path)?;
            if metadata.is_dir {
                if visited {
                    delete(&native.actor, smb_path, true)
                        .await
                        .map_err(|error| smb_network_error("delete directory", error))?;
                    continue;
                }

                stack.push((display_path.clone(), true));
                let entries = list_directory_once(&native.actor, smb_path)
                    .await
                    .map_err(|error| smb_network_error("list directory for delete", error))?;
                for entry in entries
                    .into_iter()
                    .filter(|entry| entry.file_name != "." && entry.file_name != "..")
                {
                    stack.push((child_display_path(&display_path, &entry.file_name), false));
                }
            } else {
                delete(&native.actor, smb_path, false)
                    .await
                    .map_err(|error| smb_network_error("delete file", error))?;
            }
        }

        Ok(())
    }
}

type ReadChunkFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, SmbConnectionError>> + Send>>;
type CloseFileFuture = Pin<Box<dyn Future<Output = Result<(), SmbConnectionError>> + Send>>;

struct NativeSmbReadStream {
    actor: SmbConnectionActor,
    pool: Arc<SmbPool>,
    generation: u64,
    file: Option<SmbHandle>,
    next_offset: u64,
    remaining: u64,
    buffer: Vec<u8>,
    buffer_offset: usize,
    read_future: Option<ReadChunkFuture>,
    close_future: Option<CloseFileFuture>,
    closed: bool,
}

impl NativeSmbReadStream {
    fn new(
        actor: SmbConnectionActor,
        file: SmbHandle,
        offset: u64,
        length: u64,
        initial_data: Vec<u8>,
        pool: Arc<SmbPool>,
        generation: u64,
    ) -> Self {
        Self {
            actor,
            pool,
            generation,
            file: Some(file),
            next_offset: offset.saturating_add(initial_data.len() as u64),
            remaining: length.saturating_sub(initial_data.len() as u64),
            buffer: initial_data,
            buffer_offset: 0,
            read_future: None,
            close_future: None,
            closed: false,
        }
    }

    fn poll_buffer(&mut self, read_buf: &mut ReadBuf<'_>) -> bool {
        if self.buffer_offset >= self.buffer.len() {
            self.buffer.clear();
            self.buffer_offset = 0;
            return false;
        }

        let available = self.buffer.len() - self.buffer_offset;
        let to_copy = available.min(read_buf.remaining());
        read_buf.put_slice(&self.buffer[self.buffer_offset..self.buffer_offset + to_copy]);
        self.buffer_offset += to_copy;
        true
    }

    fn start_next_read(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        const PIPELINE_DEPTH: usize = 8;
        let mut ranges = Vec::with_capacity(PIPELINE_DEPTH);
        for _ in 0..PIPELINE_DEPTH {
            if self.remaining == 0 {
                break;
            }
            let length = self.remaining.min(u64::from(DEFAULT_READ_CHUNK_LENGTH)) as u32;
            ranges.push(SmbReadRange {
                offset: self.next_offset,
                length,
            });
            self.next_offset = self.next_offset.saturating_add(u64::from(length));
            self.remaining -= u64::from(length);
        }
        let actor = self.actor.clone();
        self.read_future = Some(Box::pin(async move {
            let chunks = read_file_ranges_pipelined(&actor, &file, ranges, PIPELINE_DEPTH).await?;
            Ok(chunks.concat())
        }));
    }

    fn start_close(&mut self) {
        if self.closed || self.close_future.is_some() {
            return;
        }
        let Some(file) = self.file.take() else {
            self.closed = true;
            return;
        };
        let actor = self.actor.clone();
        self.close_future = Some(Box::pin(
            async move { close(&actor, file).await.map(|_| ()) },
        ));
    }

    fn invalidate_connection(&self) {
        self.pool.mark_invalid(self.generation);
    }
}

impl AsyncRead for NativeSmbReadStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        read_buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let stream = self.get_mut();

        loop {
            if read_buf.remaining() == 0 || stream.poll_buffer(read_buf) {
                return Poll::Ready(Ok(()));
            }

            if let Some(read_future) = stream.read_future.as_mut() {
                match read_future.as_mut().poll(cx) {
                    Poll::Ready(Ok(chunk)) => {
                        stream.read_future = None;
                        stream.buffer = chunk;
                        stream.buffer_offset = 0;
                        continue;
                    }
                    Poll::Ready(Err(error)) => {
                        stream.read_future = None;
                        stream.remaining = 0;
                        stream.start_close();
                        stream.invalidate_connection();
                        return Poll::Ready(Err(std::io::Error::other(error)));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }

            if stream.remaining > 0 {
                stream.start_next_read();
                continue;
            }

            stream.start_close();
            if let Some(close_future) = stream.close_future.as_mut() {
                match close_future.as_mut().poll(cx) {
                    Poll::Ready(Ok(())) => {
                        stream.close_future = None;
                        stream.closed = true;
                        return Poll::Ready(Ok(()));
                    }
                    Poll::Ready(Err(error)) => {
                        stream.close_future = None;
                        stream.closed = true;
                        stream.invalidate_connection();
                        return Poll::Ready(Err(std::io::Error::other(error)));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }

            return Poll::Ready(Ok(()));
        }
    }
}

impl Drop for NativeSmbReadStream {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        if let Some(file) = self.file.take() {
            let actor = self.actor.clone();
            tokio::spawn(async move {
                let _ = close(&actor, file).await;
            });
        }
    }
}

#[async_trait]
impl BackendAdapter for SmbAdapter {
    async fn connect(&self) -> Result<BackendSession, BackendError> {
        let session = self.pool.session().await?;
        Ok(BackendSession {
            backend_id: self.backend_id.clone(),
            display_name: format!("SMB: {}", session.native.tree.unc_path),
        })
    }

    async fn list_names(
        &self,
        dir: &ObjectRef,
        cursor: Option<ListCursor>,
    ) -> Result<ListPage, BackendError> {
        if cursor.is_some() {
            // Continuations are generation-bound: never reconnect/restart a page.
            let native = self.native_session().await?;
            let result = self.list_names_native(&native, dir, cursor).await;
            self.finish_mutation(&native, result).await
        } else {
            self.with_read_reconnect(|native| async move {
                self.list_names_native(&native, dir, None).await
            })
            .await
        }
    }

    async fn stat(&self, object: &ObjectRef) -> Result<MetadataResult, BackendError> {
        self.with_read_reconnect(|native| async move {
            self.stat_native(&native, object, MetadataLevel::L1).await
        })
        .await
    }

    async fn read(
        &self,
        object: &ObjectRef,
        range: Option<ByteRange>,
    ) -> Result<ReadHandle, BackendError> {
        self.with_read_reconnect(|native| {
            let range = range.clone();
            async move { self.read_native(&native, object, range).await }
        })
        .await
    }

    async fn write_object(
        &self,
        dst: &ObjectRef,
        data: Vec<u8>,
        on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        let native = self.native_session().await?;
        let conflict_result = self
            .native_conflict_display_path(&native, dst, &on_conflict)
            .await;
        let final_display_path = self.finish_mutation(&native, conflict_result).await?;
        let result = self
            .write_bytes_native(
                &native,
                &final_display_path,
                data,
                native_write_disposition(&on_conflict),
            )
            .await;
        self.finish_mutation(&native, result).await?;
        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn write_from_reader(
        &self,
        dst: &ObjectRef,
        reader: Pin<Box<dyn AsyncRead + Send>>,
        _content_length: Option<u64>,
        on_conflict: ConflictResolution,
    ) -> Result<OperationTicket, BackendError> {
        let native = self.native_session().await?;
        let conflict_result = self
            .native_conflict_display_path(&native, dst, &on_conflict)
            .await;
        let final_display_path = self.finish_mutation(&native, conflict_result).await?;
        let result = self
            .write_reader_native(
                &native,
                &final_display_path,
                reader,
                native_write_disposition(&on_conflict),
            )
            .await;
        self.finish_mutation(&native, result).await?;
        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn set_mtime(
        &self,
        object: &ObjectRef,
        mtime: std::time::SystemTime,
    ) -> Result<(), BackendError> {
        let native = self.native_session().await?;
        let path = smb_relative_path(&object.display_path)?;
        let result = set_basic_info(
            &native.actor,
            path,
            Smb2FileBasicInformation {
                // Zero leaves a time unchanged; only the write time is set.
                creation_time: 0,
                last_access_time: 0,
                last_write_time: system_time_to_filetime(mtime)?,
                change_time: 0,
                file_attributes: Smb2FileAttributes(0),
            },
        )
        .await
        .map_err(|error| smb_network_error("set modification time", error));
        self.finish_mutation(&native, result).await
    }

    async fn mkdir(&self, parent: &ObjectRef, name: &str) -> Result<ObjectRef, BackendError> {
        let native = self.native_session().await?;
        let display_path = child_display_path(&parent.display_path, name);
        let smb_path = smb_relative_path(&display_path)?;
        let result = create_directory(&native.actor, smb_path)
            .await
            .map_err(|error| smb_network_error("create directory", error));
        let directory = self.finish_mutation(&native, result).await?;
        let result = close_directory_after_create(&native.actor, directory).await;
        self.finish_mutation(&native, result).await?;
        Ok(ObjectRef {
            key: ObjectKey {
                backend_id: self.backend_id.clone(),
                stable_id: StableId(display_path.clone()),
            },
            parent_stable_id: Some(parent.key.stable_id.clone()),
            display_path,
        })
    }

    async fn copy(
        &self,
        src: &ObjectRef,
        dst: &ObjectRef,
        options: CopyOptions,
    ) -> Result<OperationTicket, BackendError> {
        let native = self.native_session().await?;
        let result = self.copy_native(&native, src, dst, &options).await;
        self.finish_mutation(&native, result).await?;
        Ok(self.next_ticket(OperationKind::Copy))
    }

    async fn move_object(
        &self,
        src: &ObjectRef,
        dst: &ObjectRef,
        options: MoveOptions,
    ) -> Result<OperationTicket, BackendError> {
        let native = self.native_session().await?;
        let result = self.move_native(&native, src, dst, &options).await;
        self.finish_mutation(&native, result).await?;
        Ok(self.next_ticket(OperationKind::Move))
    }

    async fn delete(
        &self,
        object: &ObjectRef,
        mode: DeleteMode,
    ) -> Result<OperationTicket, BackendError> {
        let native = self.native_session().await?;
        match mode {
            DeleteMode::Trash => {
                return Err(BackendError::Unsupported {
                    operation: "trash on SMB native",
                });
            }
            DeleteMode::Permanent => {
                let result = self.delete_native_recursive(&native, object).await;
                self.finish_mutation(&native, result).await?;
            }
        }
        Ok(self.next_ticket(OperationKind::Delete))
    }

    async fn get_metadata(
        &self,
        object: &ObjectRef,
        level: MetadataLevel,
    ) -> Result<MetadataResult, BackendError> {
        self.with_read_reconnect(
            |native| async move { self.stat_native(&native, object, level).await },
        )
        .await
    }

    async fn get_stable_id(&self, object: &ObjectRef) -> Result<StableId, BackendError> {
        let _native = self.native_session().await?;
        smb_relative_path(&object.display_path)?;
        Ok(StableId(object.display_path.clone()))
    }

    fn capabilities(&self) -> CapabilitySet {
        smb_capabilities()
    }
}

fn native_write_disposition(resolution: &ConflictResolution) -> Smb2CreateDisposition {
    match resolution {
        ConflictResolution::Overwrite => Smb2CreateDisposition::OverwriteIf,
        ConflictResolution::Fail | ConflictResolution::Skip | ConflictResolution::Rename => {
            Smb2CreateDisposition::Create
        }
    }
}

async fn close_directory_after_create(
    actor: &SmbConnectionActor,
    directory: SmbHandle,
) -> Result<(), BackendError> {
    close(actor, directory)
        .await
        .map(|_| ())
        .map_err(|error| smb_network_error("close created directory", error))
}

fn smb_relative_path(display_path: &str) -> Result<String, BackendError> {
    let components = checked_relative_components(display_path)?;
    Ok(components.join("\\"))
}

fn parent_display_path_and_name(display_path: &str) -> Result<(String, String), BackendError> {
    let components = checked_relative_components(display_path)?;
    let Some((name, parent)) = components.split_last() else {
        return Err(BackendError::Unsupported {
            operation: "stat SMB share root by parent lookup",
        });
    };
    Ok((parent.join("/"), name.clone()))
}

fn checked_relative_components(display_path: &str) -> Result<Vec<String>, BackendError> {
    let normalized = display_path.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(invalid_smb_path(
            display_path,
            "absolute paths are not valid SMB object paths",
        ));
    }

    let mut components = Vec::new();
    for component in normalized.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            return Err(invalid_smb_path(
                display_path,
                "parent traversal is not valid in SMB object paths",
            ));
        }
        components.push(component.to_string());
    }
    Ok(components)
}

fn child_display_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn parent_stable_id_for_display_path(display_path: &str) -> Option<StableId> {
    parent_display_path_and_name(display_path)
        .ok()
        .map(|(parent, _)| StableId(parent))
}

fn native_conflict_name(display_path: &str, attempt: u32) -> Result<String, BackendError> {
    let (parent, name) = parent_display_path_and_name(display_path)?;
    let (stem, extension) = split_extension(&name);
    Ok(child_display_path(
        &parent,
        &format!("{stem} ({attempt}){extension}"),
    ))
}

fn split_extension(name: &str) -> (&str, &str) {
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return (name, "");
    };
    if stem.is_empty() {
        (name, "")
    } else {
        (stem, extension_with_dot(name, extension))
    }
}

fn extension_with_dot<'a>(name: &'a str, extension: &str) -> &'a str {
    &name[name.len() - extension.len() - 1..]
}

fn filetime_to_system_time(filetime: u64) -> Option<SystemTime> {
    const FILETIME_UNIX_EPOCH_OFFSET_TICKS: u64 = 116_444_736_000_000_000;
    const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;

    if filetime == 0 {
        return None;
    }
    let unix_ticks = filetime.checked_sub(FILETIME_UNIX_EPOCH_OFFSET_TICKS)?;
    let seconds = unix_ticks / FILETIME_TICKS_PER_SECOND;
    let nanos = ((unix_ticks % FILETIME_TICKS_PER_SECOND) * 100) as u32;
    Some(UNIX_EPOCH + Duration::new(seconds, nanos))
}

fn system_time_to_filetime(time: SystemTime) -> Result<u64, BackendError> {
    const FILETIME_UNIX_EPOCH_OFFSET_TICKS: u64 = 116_444_736_000_000_000;
    const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;

    let duration = time.duration_since(UNIX_EPOCH).map_err(|error| {
        BackendError::Internal(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("SMB FILETIME cannot represent time before UNIX epoch: {error}"),
        )))
    })?;
    let ticks_from_seconds = duration
        .as_secs()
        .checked_mul(FILETIME_TICKS_PER_SECOND)
        .ok_or_else(|| {
            BackendError::Internal(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "SMB FILETIME seconds overflowed",
            )))
        })?;
    ticks_from_seconds
        .checked_add(u64::from(duration.subsec_nanos() / 100))
        .and_then(|ticks| ticks.checked_add(FILETIME_UNIX_EPOCH_OFFSET_TICKS))
        .ok_or_else(|| {
            BackendError::Internal(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "SMB FILETIME overflowed",
            )))
        })
}

fn invalid_smb_path(display_path: &str, reason: &'static str) -> BackendError {
    BackendError::Internal(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("invalid SMB object path '{display_path}': {reason}"),
    )))
}

fn smb_network_error(operation: &'static str, error: SmbConnectionError) -> BackendError {
    if matches!(
        &error,
        SmbConnectionError::UnexpectedStatus { .. } | SmbConnectionError::ShortWrite { .. }
    ) {
        return BackendError::Internal(Box::new(std::io::Error::other(format!(
            "SMB native {operation} failed: {error}"
        ))));
    }
    BackendError::Network(NetworkErrorDetail {
        message: format!("SMB native {operation} failed: {error}"),
        is_timeout: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_smb_client::packet::header::SMB2_HEADER_BYTES;
    use iris_smb_client::test_support::*;
    use iris_smb_client::{
        DirectTcpTransport, Smb2CloseRequest, Smb2Command, Smb2CreateDisposition,
        Smb2CreateOptions, Smb2CreateRequest, Smb2DesiredAccess, Smb2Dialect, Smb2FileId,
        Smb2FileInformationClass, Smb2Header, Smb2IoctlFlags, Smb2IoctlRequest,
        Smb2NegotiateRequest, Smb2QueryDirectoryFlags, Smb2QueryDirectoryRequest, Smb2ReadRequest,
        Smb2SetInfoRequest, Smb2ShareFlags, Smb2TreeConnectRequest, Smb2WriteRequest, SmbNtStatus,
        DEFAULT_COPYCHUNK_CHUNK_BYTES, FSCTL_SRV_COPYCHUNK, FSCTL_SRV_REQUEST_RESUME_KEY,
        SMB2_RESUME_KEY_BYTES,
    };
    use iris_smb_client::{Smb2FileInfoClass, Smb2InfoType};
    use proptest::prelude::*;
    use std::io::Cursor;
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    fn object_ref(backend_id: &BackendId, display_path: &str) -> ObjectRef {
        ObjectRef {
            key: ObjectKey {
                backend_id: backend_id.clone(),
                stable_id: StableId(display_path.to_string()),
            },
            parent_stable_id: None,
            display_path: display_path.to_string(),
        }
    }

    proptest! {
        #[test]
        fn smb_relative_path_normalizes_separators(
            components in prop::collection::vec("[A-Za-z0-9_-]{1,12}", 0..16),
            use_backslash in any::<bool>(),
        ) {
            let separator = if use_backslash { "\\" } else { "/" };
            let display_path = components.join(separator);

            prop_assert_eq!(
                smb_relative_path(&display_path).expect("relative components should be valid"),
                components.join("\\")
            );
        }

        #[test]
        fn smb_relative_path_rejects_parent_traversal(
            prefix in prop::collection::vec("[A-Za-z0-9_-]{1,12}", 0..8),
            suffix in prop::collection::vec("[A-Za-z0-9_-]{1,12}", 0..8),
        ) {
            let mut components = prefix;
            components.push("..".to_string());
            components.extend(suffix);

            prop_assert!(smb_relative_path(&components.join("/")).is_err());
        }
    }

    #[test]
    fn smb_status_errors_do_not_trigger_connection_retries() {
        let status = smb_network_error(
            "probe",
            SmbConnectionError::UnexpectedStatus {
                command: Smb2Command::Create,
                status: 0xc000_0022,
            },
        );
        let disconnect = smb_network_error(
            "probe",
            SmbConnectionError::ActorStopped {
                reason: "connection closed".to_string(),
            },
        );

        assert!(matches!(status, BackendError::Internal(_)));
        assert!(matches!(disconnect, BackendError::Network(_)));
    }

    #[tokio::test]
    async fn native_listing_is_incremental_and_cursor_is_single_use() {
        let counts = Arc::new(ListingCounts::default());
        let (mut config, server) =
            start_fake_native_server(FakeNativeScenario::Incremental(counts.clone())).await;
        assert_eq!(config.listing.page_size.get(), 100);
        config.listing.page_size = std::num::NonZeroUsize::new(2).unwrap();
        let backend = BackendId("smb:pages".into());
        let adapter = SmbAdapter::new(config, backend.clone());
        let root = object_ref(&backend, "");
        let first = adapter.list_names(&root, None).await.unwrap();
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.name().name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        assert_eq!(counts.creates.load(Ordering::SeqCst), 1);
        assert_eq!(
            counts.queries.load(Ordering::SeqCst),
            1,
            "page one must not fetch the whole directory"
        );
        let token = first.next_cursor.unwrap();
        assert!(token.0.parse::<usize>().is_err());
        let second = adapter
            .list_names(&root, Some(token.clone()))
            .await
            .unwrap();
        assert_eq!(
            second
                .entries
                .iter()
                .map(|entry| entry.name().name.as_str())
                .collect::<Vec<_>>(),
            ["gamma", "delta"]
        );
        let gamma = second.entries[0].name();
        assert!(gamma.is_dir);
        assert!(!gamma.is_symlink && !gamma.is_inaccessible);
        assert_eq!(
            gamma.object_ref.parent_stable_id,
            Some(root.key.stable_id.clone())
        );
        assert_eq!(gamma.object_ref.key.backend_id, backend);
        assert_eq!(gamma.object_ref.display_path, "gamma");
        assert!(
            adapter.list_names(&root, Some(token)).await.is_err(),
            "replay rejected"
        );
        let last = adapter.list_names(&root, second.next_cursor).await.unwrap();
        assert_eq!(
            last.entries
                .iter()
                .map(|entry| entry.name().name.as_str())
                .collect::<Vec<_>>(),
            ["epsilon"]
        );
        assert!(last.next_cursor.is_none());
        assert_eq!(counts.creates.load(Ordering::SeqCst), 1);
        assert_eq!(counts.queries.load(Ordering::SeqCst), 3);
        assert_eq!(counts.closes.load(Ordering::SeqCst), 1);
        drop(adapter);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn failed_cursor_eviction_close_does_not_return_a_dead_cursor() {
        let counts = Arc::new(ListingCounts::default());
        let (mut config, server) =
            start_fake_native_server(FakeNativeScenario::Incremental(counts.clone())).await;
        config.listing.page_size = std::num::NonZeroUsize::new(1).unwrap();
        config.listing.max_active = std::num::NonZeroUsize::new(1).unwrap();
        let backend = BackendId("smb:close-failure".into());
        let adapter = SmbAdapter::new(config, backend.clone());
        let root = object_ref(&backend, "");
        let first = adapter.list_names(&root, None).await.unwrap();
        assert!(first.next_cursor.is_some());

        counts.fail_close.store(true, Ordering::SeqCst);
        assert!(matches!(
            adapter.list_names(&root, None).await,
            Err(BackendError::Network(_))
        ));
        assert!(adapter.list_cursors.lock().unwrap().is_empty());
        drop(adapter);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn native_cursors_close_on_cap_expiry_wrong_directory_and_generation() {
        let counts = Arc::new(ListingCounts::default());
        let (mut config, server) =
            start_fake_native_server(FakeNativeScenario::Incremental(counts.clone())).await;
        assert_eq!(config.listing.max_active.get(), 64);
        assert_eq!(config.listing.idle_timeout, Duration::from_secs(30));
        config.listing.page_size = std::num::NonZeroUsize::new(1).unwrap();
        config.listing.max_active = std::num::NonZeroUsize::new(1).unwrap();
        let backend = BackendId("smb:cleanup".into());
        let adapter = SmbAdapter::new(config, backend.clone());
        let root = object_ref(&backend, "");
        let first = adapter
            .list_names(&root, None)
            .await
            .unwrap()
            .next_cursor
            .unwrap();
        let second = adapter
            .list_names(&root, None)
            .await
            .unwrap()
            .next_cursor
            .unwrap();
        assert_eq!(
            counts.closes.load(Ordering::SeqCst),
            1,
            "oldest cursor evicted"
        );
        assert_eq!(adapter.list_cursors.lock().unwrap().len(), 1);
        assert!(adapter.list_names(&root, Some(first)).await.is_err());
        assert!(adapter
            .list_names(&object_ref(&backend, "other"), Some(second))
            .await
            .is_err());
        assert_eq!(
            counts.closes.load(Ordering::SeqCst),
            2,
            "wrong-directory cursor closed"
        );
        let third = adapter
            .list_names(&root, None)
            .await
            .unwrap()
            .next_cursor
            .unwrap();
        // Set policy to immediate expiry, without timing-dependent sleeps.
        let mut adapter = adapter;
        adapter.listing.idle_timeout = Duration::ZERO;
        assert!(adapter.list_names(&root, Some(third)).await.is_err());
        assert_eq!(counts.closes.load(Ordering::SeqCst), 3);
        adapter.listing.idle_timeout = Duration::from_secs(30);
        let fourth = adapter
            .list_names(&root, None)
            .await
            .unwrap()
            .next_cursor
            .unwrap();
        let mut native = adapter.native_session().await.unwrap();
        native.generation += 1;
        assert!(adapter
            .list_names_native(&native, &root, Some(fourth))
            .await
            .is_err());
        assert_eq!(counts.closes.load(Ordering::SeqCst), 4);
        drop(adapter);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn native_stat_opens_exact_child_and_preserves_metadata() {
        let (config, server) = start_fake_native_server(FakeNativeScenario::StatMetadata).await;
        let backend = BackendId("smb:stat".into());
        let adapter = SmbAdapter::new(config, backend.clone());
        for (path, is_dir, size, content_type) in [
            (
                "folder/report.pdf",
                false,
                Some(512),
                Some("application/pdf".to_string()),
            ),
            ("folder/child", true, None, None),
        ] {
            let metadata = adapter.stat(&object_ref(&backend, path)).await.unwrap();
            assert_eq!(metadata.level, MetadataLevel::L1);
            assert_eq!(metadata.is_dir, is_dir);
            assert_eq!(metadata.size, size);
            assert_eq!(metadata.ctime, Some(UNIX_EPOCH + Duration::from_secs(1)));
            assert_eq!(metadata.mtime, Some(UNIX_EPOCH + Duration::from_secs(2)));
            assert_eq!(metadata.content_type, content_type);
            assert!(metadata.etag.is_none());
        }
        assert!(matches!(
            adapter.stat(&object_ref(&backend, "folder/missing")).await,
            Err(BackendError::NotFound { .. })
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn native_list_names_uses_smb_actor_session() {
        let (config, server_task) = start_fake_native_server(FakeNativeScenario::ListRoot).await;
        let backend_id = BackendId("smb:native".to_string());
        let adapter = SmbAdapter::new(config, backend_id.clone());

        let session = adapter.connect().await.expect("connect");
        assert_eq!(session.display_name, "SMB: \\\\127.0.0.1\\share");

        let page = adapter
            .list_names(&object_ref(&backend_id, ""), None)
            .await
            .expect("list");

        assert_eq!(page.entries.len(), 1);
        let entry = page.entries[0].name();
        assert_eq!(entry.name, "report.pdf");
        assert!(!entry.is_dir);
        assert_eq!(page.entries[0].name().object_ref.display_path, "report.pdf");
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn native_list_reconnects_after_transport_disconnect() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let server_task = tokio::spawn(async move {
            let (first_stream, _) = listener.accept().await.expect("first accept");
            let mut first = DirectTcpTransport::new(first_stream);
            handle_negotiate(&mut first).await;
            handle_tree_connect(&mut first).await;
            let request = first.recv_packet().await.expect("failed list request");
            let header = Smb2Header::decode(&request).expect("failed list header");
            assert_eq!(header.command, Smb2Command::Create);
            drop(first);

            let (second_stream, _) = listener.accept().await.expect("second accept");
            let mut second = DirectTcpTransport::new(second_stream);
            handle_negotiate(&mut second).await;
            handle_tree_connect(&mut second).await;
            handle_list_root(&mut second).await;
        });
        let config = SmbConnectionConfig {
            server: addr.ip().to_string(),
            port: addr.port(),
            share_name: "share".to_string(),
            username: None,
            domain: None,
            password: None,
            workstation: None,
            allow_guest: true,
            actor: Default::default(),
            listing: Default::default(),
        };
        let backend_id = BackendId("smb:reconnect".to_string());
        let adapter = SmbAdapter::new(config, backend_id.clone());
        let first_generation = adapter
            .pool
            .session()
            .await
            .expect("first session")
            .native
            .generation;

        let page = adapter
            .list_names(&object_ref(&backend_id, ""), None)
            .await
            .expect("reconnected list");

        assert_eq!(page.entries.len(), 1);
        let second_generation = adapter
            .pool
            .session()
            .await
            .expect("second session")
            .native
            .generation;
        assert_ne!(first_generation, second_generation);
        adapter.pool.invalidate(first_generation).await;
        assert_eq!(
            adapter
                .pool
                .session()
                .await
                .expect("new session remains cached")
                .native
                .generation,
            second_generation
        );
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn native_read_uses_smb_actor_session() {
        let (config, server_task) = start_fake_native_server(FakeNativeScenario::ReadFile).await;
        let backend_id = BackendId("smb:native".to_string());
        let adapter = SmbAdapter::new(config, backend_id.clone());

        let mut handle = adapter
            .read(&object_ref(&backend_id, "folder/file.bin"), None)
            .await
            .expect("read");
        let mut data = Vec::new();
        handle
            .stream
            .read_to_end(&mut data)
            .await
            .expect("read stream");

        assert_eq!(data.len(), DEFAULT_READ_CHUNK_LENGTH as usize + 4);
        assert!(data[..DEFAULT_READ_CHUNK_LENGTH as usize]
            .iter()
            .all(|byte| *byte == b'A'));
        assert_eq!(&data[DEFAULT_READ_CHUNK_LENGTH as usize..], b"TAIL");
        assert_eq!(
            handle.content_length,
            Some(u64::from(DEFAULT_READ_CHUNK_LENGTH) + 4)
        );
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn native_write_from_reader_uses_smb_actor_session() {
        let (config, server_task) = start_fake_native_server(FakeNativeScenario::WriteFile).await;
        let backend_id = BackendId("smb:native".to_string());
        let adapter = SmbAdapter::new(config, backend_id.clone());

        adapter
            .write_from_reader(
                &object_ref(&backend_id, "folder/out.bin"),
                Box::pin(Cursor::new(b"native payload".to_vec())),
                Some(14),
                ConflictResolution::Fail,
            )
            .await
            .expect("write from reader");

        server_task.await.expect("server task");
    }

    struct FailingReader;

    impl AsyncRead for FailingReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "synthetic source failure",
            )))
        }
    }

    struct PendingReader(Option<tokio::sync::oneshot::Sender<()>>);

    impl AsyncRead for PendingReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if let Some(entered) = self.0.take() {
                entered.send(()).expect("reader polled after OPEN");
            }
            Poll::Pending
        }
    }

    #[tokio::test]
    async fn native_streamed_reader_error_closes_and_preserves_error() {
        tokio::time::timeout(Duration::from_secs(5), async {
            for close_fails in [false, true] {
                let (config, server) =
                    start_fake_native_server(FakeNativeScenario::StreamedWrite(Some(close_fails)))
                        .await;
                let backend_id = BackendId("smb:native".to_string());
                let adapter = SmbAdapter::new(config, backend_id.clone());
                let generation = adapter.native_session().await.unwrap().generation;
                let error = adapter
                    .write_from_reader(
                        &object_ref(&backend_id, "folder/out.bin"),
                        Box::pin(FailingReader),
                        None,
                        ConflictResolution::Fail,
                    )
                    .await
                    .expect_err("source failed");
                let BackendError::Internal(error) = error else {
                    panic!("original source error was masked: {error:?}");
                };
                let error = error.downcast_ref::<std::io::Error>().unwrap();
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
                assert_eq!(error.to_string(), "synthetic source failure");

                // A successful CLOSE keeps the same connection; cleanup failure
                // must evict it even though the returned error is Internal.
                let next = adapter.native_session().await.unwrap();
                assert_eq!(next.generation == generation, !close_fails);
                adapter
                    .list_names(&object_ref(&backend_id, ""), None)
                    .await
                    .expect("follow-up listing");
                server.await.expect("server task");
            }
        })
        .await
        .expect("bounded reader failure regression");
    }

    #[test]
    fn streamed_write_guard_can_drop_without_runtime() {
        let pool = Arc::new(SmbPool::new(SmbConnectionConfig {
            server: "127.0.0.1".to_string(),
            port: 445,
            share_name: "share".to_string(),
            username: None,
            domain: None,
            password: None,
            workstation: None,
            allow_guest: true,
            actor: Default::default(),
            listing: Default::default(),
        }));
        drop(StreamedWriteGuard {
            pool,
            generation: 1,
            armed: true,
        });
    }

    #[tokio::test]
    async fn native_streamed_write_cancellation_evicts_session() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (config, server) =
                start_fake_native_server(FakeNativeScenario::StreamedWrite(None)).await;
            let backend_id = BackendId("smb:native".to_string());
            let adapter = SmbAdapter::new(config, backend_id.clone());
            let generation = adapter.native_session().await.unwrap().generation;
            let (entered, ready) = tokio::sync::oneshot::channel();
            let dst = object_ref(&backend_id, "folder/out.bin");
            let mut write = Box::pin(adapter.write_from_reader(
                &dst,
                Box::pin(PendingReader(Some(entered))),
                None,
                ConflictResolution::Fail,
            ));
            tokio::select! {
                result = &mut write => panic!("unexpected write result: {result:?}"),
                result = ready => result.expect("source reached after OPEN"),
            }
            drop(write);
            // No yield between cancellation and acquisition: the synchronous
            // invalid marker, not the background removal task, prevents reuse.
            let next = adapter.native_session().await.unwrap();
            assert_ne!(next.generation, generation);
            adapter
                .list_names(&object_ref(&backend_id, ""), None)
                .await
                .expect("listing on new connection");
            server
                .await
                .expect("old connection closed and new connection used");
        })
        .await
        .expect("bounded cancellation regression");
    }

    #[tokio::test]
    async fn native_pool_invalidation_stops_stale_clones_and_reconnects() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (mut config, server) =
                start_fake_native_server(FakeNativeScenario::PoolInvalidation).await;
            config.actor.request_timeout = Duration::from_millis(500);
            let backend_id = BackendId("smb:pool-lifecycle".into());
            let adapter = SmbAdapter::new(config, backend_id.clone());
            let stale = adapter.native_session().await.unwrap();
            assert!(matches!(
                stale
                    .actor
                    .request_with_credit_charge(Smb2Command::Read, vec![], u16::MAX)
                    .await,
                Err(SmbConnectionError::RequestTimedOut { .. })
            ));
            adapter.pool.invalidate(stale.generation).await;
            assert!(stale.actor.state().await.is_err());
            let replacement = adapter.native_session().await.unwrap();
            assert!(replacement.generation > stale.generation);
            // A delayed invalidation from a stale owner cannot kill the replacement.
            adapter.pool.invalidate(stale.generation).await;
            replacement.actor.state().await.unwrap();
            adapter
                .list_names(&object_ref(&backend_id, ""), None)
                .await
                .unwrap();
            server.await.unwrap();
        })
        .await
        .expect("bounded pool lifecycle regression");
    }

    #[tokio::test]
    async fn native_streamed_inflight_writes_never_return_to_pool() {
        tokio::time::timeout(Duration::from_secs(5), async {
            for fail_write in [false, true] {
                let (entered, ready) = tokio::sync::oneshot::channel();
                let (config, server) =
                    start_fake_native_server(FakeNativeScenario::StreamedPipeline {
                        fail_write,
                        entered,
                    })
                    .await;
                let backend_id = BackendId("smb:native".to_string());
                let adapter = SmbAdapter::new(config, backend_id.clone());
                let generation = adapter.native_session().await.unwrap().generation;
                let dst = object_ref(&backend_id, "folder/out.bin");
                let mut write = Box::pin(adapter.write_from_reader(
                    &dst,
                    Box::pin(Cursor::new(vec![42; 2 * DEFAULT_WRITE_CHUNK_LENGTH])),
                    None,
                    ConflictResolution::Fail,
                ));
                if fail_write {
                    let error = write.as_mut().await.expect_err("first write failed");
                    let BackendError::Internal(error) = error else {
                        panic!("original write error was masked: {error:?}");
                    };
                    assert_eq!(
                        error.to_string(),
                        format!(
                            "SMB native streaming write failed: {}",
                            SmbConnectionError::ShortWrite {
                                expected: DEFAULT_WRITE_CHUNK_LENGTH as u32,
                                actual: 0,
                            }
                        )
                    );
                } else {
                    tokio::select! {
                        result = &mut write => panic!("unexpected write result: {result:?}"),
                        result = ready => result.expect("two dispatched writes"),
                    }
                }
                drop(write);
                let next = adapter.native_session().await.unwrap();
                assert_ne!(next.generation, generation);
                adapter
                    .list_names(&object_ref(&backend_id, ""), None)
                    .await
                    .expect("new session works while old writes are unresolved");
                server
                    .await
                    .expect("old generation closes without draining writes");
            }
        })
        .await
        .expect("bounded in-flight write regression");
    }

    #[tokio::test]
    async fn native_mkdir_uses_smb_actor_session() {
        let (config, server_task) = start_fake_native_server(FakeNativeScenario::Mkdir).await;
        let backend_id = BackendId("smb:native".to_string());
        let adapter = SmbAdapter::new(config, backend_id.clone());

        let created = adapter
            .mkdir(&object_ref(&backend_id, "folder"), "new-dir")
            .await
            .expect("mkdir");

        assert_eq!(created.display_path, "folder/new-dir");
        assert_eq!(
            created.parent_stable_id,
            Some(StableId("folder".to_string()))
        );
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn native_move_object_uses_smb_set_info_rename() {
        let (config, server_task) = start_fake_native_server(FakeNativeScenario::MoveFile).await;
        let backend_id = BackendId("smb:native".to_string());
        let adapter = SmbAdapter::new(config, backend_id.clone());

        adapter
            .move_object(
                &object_ref(&backend_id, "file.bin"),
                &object_ref(&backend_id, "renamed.bin"),
                MoveOptions {
                    on_conflict: ConflictResolution::Fail,
                },
            )
            .await
            .expect("move");

        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn native_delete_uses_smb_set_info_disposition() {
        let (config, server_task) = start_fake_native_server(FakeNativeScenario::DeleteFile).await;
        let backend_id = BackendId("smb:native".to_string());
        let adapter = SmbAdapter::new(config, backend_id.clone());

        adapter
            .delete(&object_ref(&backend_id, "file.bin"), DeleteMode::Permanent)
            .await
            .expect("delete");

        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn native_copy_uses_smb_copychunk_and_preserves_mtime() {
        let (config, server_task) = start_fake_native_server(FakeNativeScenario::CopyFile).await;
        let backend_id = BackendId("smb:native".to_string());
        let adapter = SmbAdapter::new(config, backend_id.clone());

        adapter
            .copy(
                &object_ref(&backend_id, "file.bin"),
                &object_ref(&backend_id, "copy.bin"),
                CopyOptions {
                    on_conflict: ConflictResolution::Fail,
                },
            )
            .await
            .expect("copy");

        server_task.await.expect("server task");
    }

    #[derive(Default)]
    struct ListingCounts {
        creates: AtomicU64,
        queries: AtomicU64,
        closes: AtomicU64,
        fail_close: std::sync::atomic::AtomicBool,
    }

    enum FakeNativeScenario {
        Incremental(Arc<ListingCounts>),
        StatMetadata,
        PoolInvalidation,
        ListRoot,
        ReadFile,
        WriteFile,
        // None cancels while the source is pending; Some controls CLOSE status.
        StreamedWrite(Option<bool>),
        StreamedPipeline {
            fail_write: bool,
            entered: tokio::sync::oneshot::Sender<()>,
        },
        Mkdir,
        MoveFile,
        DeleteFile,
        CopyFile,
    }

    async fn start_fake_native_server(
        scenario: FakeNativeScenario,
    ) -> (SmbConnectionConfig, JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut transport = DirectTcpTransport::new(stream);
            handle_negotiate(&mut transport).await;
            handle_tree_connect(&mut transport).await;
            match scenario {
                FakeNativeScenario::PoolInvalidation => {
                    // No response or owner drop releases this generation: shutdown must.
                    assert!(
                        transport.recv_packet().await.is_err(),
                        "stale generation reused"
                    );
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut replacement = DirectTcpTransport::new(stream);
                    handle_negotiate(&mut replacement).await;
                    handle_tree_connect(&mut replacement).await;
                    handle_list_root(&mut replacement).await;
                }
                FakeNativeScenario::Incremental(counts) => {
                    handle_incremental_listing(&mut transport, &counts).await
                }
                FakeNativeScenario::StatMetadata => {
                    for (name, attributes, size) in [
                        ("folder/report.pdf", Smb2FileAttributes::ARCHIVE, 512),
                        ("folder/child", Smb2FileAttributes::DIRECTORY, 0),
                    ] {
                        let (parent, name) = parent_display_path_and_name(name).unwrap();
                        handle_stat_found(
                            &mut transport,
                            &parent,
                            directory_entry_record(0, &name, size, 42, attributes),
                        )
                        .await;
                    }
                    handle_stat_missing(&mut transport, "folder\\missing").await;
                }
                FakeNativeScenario::ListRoot => handle_list_root(&mut transport).await,
                FakeNativeScenario::ReadFile => handle_read_file(&mut transport).await,
                FakeNativeScenario::WriteFile => handle_write_file(&mut transport).await,
                FakeNativeScenario::StreamedWrite(close_fails) => {
                    handle_stat_missing(&mut transport, "folder\\out.bin").await;
                    handle_streamed_write_open(&mut transport).await;
                    if let Some(fails) = close_fails {
                        let packet = transport.recv_packet().await.expect("streamed CLOSE");
                        let header = Smb2Header::decode(&packet).unwrap();
                        assert_eq!(header.command, Smb2Command::Close);
                        let body = Smb2CloseRequest::decode(&packet[SMB2_HEADER_BYTES..]).unwrap();
                        assert_eq!(body.file_id, fake_file_id());
                        let mut response = Smb2Header::new(Smb2Command::Close, header.message_id);
                        if fails {
                            response.status_or_channel_sequence = 0xc000_0022; // STATUS_ACCESS_DENIED
                        }
                        transport
                            .send_packet(&response_packet(
                                response,
                                &if fails {
                                    vec![9, 0, 0, 0, 0, 0, 0, 0, 0] // SMB2 ERROR response
                                } else {
                                    close_response_body(Smb2FileAttributes::ARCHIVE, 0)
                                },
                            ))
                            .await
                            .unwrap();
                    }
                    if close_fails != Some(false) {
                        // No cancellation CLOSE: invalidation tears down the generation
                        // even while callers retain actor clones.
                        assert!(transport.recv_packet().await.is_err(), "old actor reused");
                        let (stream, _) = listener.accept().await.expect("replacement connection");
                        transport = DirectTcpTransport::new(stream);
                        handle_negotiate(&mut transport).await;
                        handle_tree_connect(&mut transport).await;
                    }
                    handle_list_root(&mut transport).await;
                }
                FakeNativeScenario::StreamedPipeline {
                    fail_write,
                    entered,
                } => {
                    handle_stat_missing(&mut transport, "folder\\out.bin").await;
                    handle_streamed_write_open(&mut transport).await;
                    let mut writes = Vec::new();
                    for _ in 0..2 {
                        let packet = transport.recv_packet().await.expect("pipelined write");
                        let header = Smb2Header::decode(&packet).unwrap();
                        assert_eq!(header.command, Smb2Command::Write);
                        let body = Smb2WriteRequest::decode(&packet[SMB2_HEADER_BYTES..]).unwrap();
                        assert_eq!(body.file_id, fake_file_id());
                        writes.push((body.offset, header.message_id, body.data.len() as u32));
                    }
                    writes.sort_by_key(|write| write.0);
                    entered.send(()).expect("writes observed");
                    if fail_write {
                        // Fail the first task but hold its sibling until the
                        // replacement generation has completed useful work.
                        transport
                            .send_packet(&response_packet(
                                Smb2Header::new(Smb2Command::Write, writes.remove(0).1),
                                &write_response_body(0),
                            ))
                            .await
                            .unwrap();
                    }
                    // Invalidation must not wait for the unresolved sibling WRITE.
                    assert!(transport.recv_packet().await.is_err(), "old actor reused");
                    let (stream, _) = listener.accept().await.expect("replacement connection");
                    let mut replacement = DirectTcpTransport::new(stream);
                    handle_negotiate(&mut replacement).await;
                    handle_tree_connect(&mut replacement).await;
                    handle_list_root(&mut replacement).await;
                }
                FakeNativeScenario::Mkdir => handle_mkdir(&mut transport).await,
                FakeNativeScenario::MoveFile => handle_move_file(&mut transport).await,
                FakeNativeScenario::DeleteFile => handle_delete_file(&mut transport).await,
                FakeNativeScenario::CopyFile => handle_copy_file(&mut transport).await,
            }
        });
        (
            SmbConnectionConfig {
                server: addr.ip().to_string(),
                port: addr.port(),
                share_name: "share".to_string(),
                username: None,
                domain: None,
                password: None,
                workstation: None,
                allow_guest: true,
                actor: Default::default(),
                listing: Default::default(),
            },
            server_task,
        )
    }

    async fn handle_negotiate<T>(transport: &mut DirectTcpTransport<T>)
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let request = transport.recv_packet().await.expect("negotiate");
        let header = Smb2Header::decode(&request).expect("negotiate header");
        assert_eq!(header.command, Smb2Command::Negotiate);
        let body =
            Smb2NegotiateRequest::decode(&request[SMB2_HEADER_BYTES..]).expect("negotiate request");
        assert_eq!(
            body.dialects,
            vec![
                Smb2Dialect::Smb311,
                Smb2Dialect::Smb302,
                Smb2Dialect::Smb300,
                Smb2Dialect::Smb210,
            ]
        );
        assert_eq!(body.contexts.len(), 2);

        assert_eq!(header.credit_request_response, 256);
        let mut response_header = Smb2Header::new(Smb2Command::Negotiate, header.message_id);
        response_header.credit_request_response = 256;
        transport
            .send_packet(&response_packet(
                response_header,
                &negotiate_response_body(),
            ))
            .await
            .expect("negotiate response");
    }

    async fn handle_tree_connect<T>(transport: &mut DirectTcpTransport<T>)
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let mut request = transport
            .recv_packet()
            .await
            .expect("session or tree connect");
        let mut header = Smb2Header::decode(&request).expect("session or tree header");
        if header.command == Smb2Command::SessionSetup {
            let mut response_header = Smb2Header::new(Smb2Command::SessionSetup, header.message_id);
            response_header.session_id = 0x5566_7788_99aa_bbcc;
            transport
                .send_packet(&response_packet(
                    response_header,
                    &[9, 0, 1, 0, 72, 0, 0, 0],
                ))
                .await
                .expect("guest session response");
            request = transport.recv_packet().await.expect("tree connect");
            header = Smb2Header::decode(&request).expect("tree header");
            assert_eq!(header.session_id, 0x5566_7788_99aa_bbcc);
        }
        assert_eq!(header.command, Smb2Command::TreeConnect);
        let tree =
            Smb2TreeConnectRequest::decode(&request[SMB2_HEADER_BYTES..]).expect("tree request");
        assert_eq!(tree.path, "\\\\127.0.0.1\\share");

        let mut response_header = Smb2Header::new(Smb2Command::TreeConnect, header.message_id);
        response_header.tree_id = 0x1122_3344;
        transport
            .send_packet(&response_packet(
                response_header,
                &tree_connect_response_body(Smb2ShareFlags::DFS),
            ))
            .await
            .expect("tree response");
    }

    async fn handle_incremental_listing<T>(
        transport: &mut DirectTcpTransport<T>,
        counts: &ListingCounts,
    ) where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let mut handles = HashMap::new();
        while let Ok(request) = transport.recv_packet().await {
            let header = Smb2Header::decode(&request).unwrap();
            let mut response = Smb2Header::new(header.command, header.message_id);
            let body = &request[SMB2_HEADER_BYTES..];
            let body = match header.command {
                Smb2Command::Create => {
                    let create = Smb2CreateRequest::decode(body).unwrap();
                    assert!(create
                        .create_options
                        .contains(Smb2CreateOptions::DIRECTORY_FILE));
                    let id = counts.creates.fetch_add(1, Ordering::SeqCst) + 1;
                    handles.insert(id, 0);
                    create_response_body(
                        Smb2FileId {
                            persistent: id,
                            volatile: id,
                        },
                        Smb2FileAttributes::DIRECTORY,
                        0,
                    )
                }
                Smb2Command::QueryDirectory => {
                    counts.queries.fetch_add(1, Ordering::SeqCst);
                    let query = Smb2QueryDirectoryRequest::decode(body).unwrap();
                    assert_eq!(query.file_index, 0); // Never an index-based resume.
                    let page = handles
                        .get_mut(&query.file_id.persistent)
                        .expect("same open handle");
                    assert_eq!(
                        query.flags.contains(Smb2QueryDirectoryFlags::RESTART_SCANS),
                        *page == 0
                    );
                    let names: &[&str] = match *page {
                        0 => &[".", "..", "alpha", "beta", "gamma"],
                        1 => &["delta", "epsilon"],
                        _ => &[],
                    };
                    *page += 1;
                    if names.is_empty() {
                        response.status_or_channel_sequence = SmbNtStatus::NO_MORE_FILES.0;
                        Vec::new()
                    } else {
                        let mut buffer = Vec::new();
                        for (index, name) in names.iter().enumerate() {
                            // Deliberately identical file_index values across all records.
                            let mut record = directory_entry_record(
                                7,
                                name,
                                42,
                                42,
                                if *name == "gamma" {
                                    Smb2FileAttributes::DIRECTORY
                                } else {
                                    Smb2FileAttributes::ARCHIVE
                                },
                            );
                            if index + 1 < names.len() {
                                let len = record.len() as u32;
                                put_u32(&mut record, 0, len);
                            }
                            buffer.extend(record);
                        }
                        query_directory_response_body(&buffer)
                    }
                }
                Smb2Command::Close => {
                    counts.closes.fetch_add(1, Ordering::SeqCst);
                    let close = Smb2CloseRequest::decode(body).unwrap();
                    assert!(
                        handles.remove(&close.file_id.persistent).is_some(),
                        "closed exactly once"
                    );
                    if counts.fail_close.load(Ordering::SeqCst) {
                        response.status_or_channel_sequence = 0xc000_0022;
                        vec![9, 0, 0, 0, 0, 0, 0, 0, 0]
                    } else {
                        close_response_body(Smb2FileAttributes::DIRECTORY, 0)
                    }
                }
                command => panic!("unexpected listing command {command:?}"),
            };
            transport
                .send_packet(&response_packet(response, &body))
                .await
                .unwrap();
        }
    }

    async fn handle_list_root<T>(transport: &mut DirectTcpTransport<T>)
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let file_id = fake_file_id();
        for (step, expected) in [
            Smb2Command::Create,
            Smb2Command::QueryDirectory,
            Smb2Command::QueryDirectory,
            Smb2Command::Close,
        ]
        .into_iter()
        .enumerate()
        {
            let request = transport.recv_packet().await.expect("list request");
            let header = Smb2Header::decode(&request).expect("list header");
            assert_eq!(header.command, expected);
            assert_eq!(header.tree_id, 0x1122_3344);
            let body = &request[SMB2_HEADER_BYTES..];
            let response_body = match expected {
                Smb2Command::Create => {
                    let create = Smb2CreateRequest::decode(body).expect("create");
                    assert_eq!(create.name, "");
                    assert!(create
                        .create_options
                        .contains(Smb2CreateOptions::DIRECTORY_FILE));
                    create_response_body(file_id, Smb2FileAttributes::DIRECTORY, 0)
                }
                Smb2Command::QueryDirectory => {
                    let query = Smb2QueryDirectoryRequest::decode(body).expect("query");
                    assert_eq!(
                        query.file_information_class,
                        Smb2FileInformationClass::FILE_ID_BOTH_DIRECTORY_INFORMATION
                    );
                    assert_eq!(
                        query.flags.contains(Smb2QueryDirectoryFlags::RESTART_SCANS),
                        step == 1
                    );
                    assert_eq!(query.file_id, file_id);
                    if step == 1 {
                        query_directory_response_body(&directory_entry_record(
                            7,
                            "report.pdf",
                            512,
                            0x777,
                            Smb2FileAttributes::ARCHIVE,
                        ))
                    } else {
                        Vec::new()
                    }
                }
                Smb2Command::Close => {
                    let close = Smb2CloseRequest::decode(body).expect("close");
                    assert_eq!(close.file_id, file_id);
                    close_response_body(Smb2FileAttributes::DIRECTORY, 0)
                }
                _ => unreachable!("covered command list"),
            };
            let mut response_header = Smb2Header::new(expected, header.message_id);
            if step == 2 {
                response_header.status_or_channel_sequence = SmbNtStatus::NO_MORE_FILES.0;
            }
            transport
                .send_packet(&response_packet(response_header, &response_body))
                .await
                .expect("list response");
        }
    }

    async fn handle_read_file<T>(transport: &mut DirectTcpTransport<T>)
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let file_id = fake_file_id();
        let eof = u64::from(DEFAULT_READ_CHUNK_LENGTH) + 4;

        let create = transport.recv_packet().await.expect("create/read request");
        let create_header = Smb2Header::decode(&create).expect("create header");
        assert_eq!(create_header.command, Smb2Command::Create);
        assert_eq!(create_header.tree_id, 0x1122_3344);
        let read_offset = create_header.next_command as usize;
        let first_header = Smb2Header::decode(&create[read_offset..]).expect("first read header");
        assert_eq!(first_header.command, Smb2Command::Read);
        let create_body =
            Smb2CreateRequest::decode(&create[SMB2_HEADER_BYTES..read_offset]).expect("create");
        assert_eq!(create_body.name, "folder\\file.bin");
        let first_body = Smb2ReadRequest::decode(&create[read_offset + SMB2_HEADER_BYTES..])
            .expect("first read");
        assert_eq!(first_body.file_id.persistent, u64::MAX);
        assert_eq!(first_body.file_id.volatile, u64::MAX);
        assert_eq!(first_body.offset, 0);
        assert_eq!(first_body.length, DEFAULT_READ_CHUNK_LENGTH);
        transport
            .send_packet(&compound_response_packet(vec![
                (
                    Smb2Header::new(Smb2Command::Create, create_header.message_id),
                    create_response_body(file_id, Smb2FileAttributes::ARCHIVE, eof),
                ),
                (
                    Smb2Header::new(Smb2Command::Read, first_header.message_id),
                    read_response_body(&vec![b'A'; DEFAULT_READ_CHUNK_LENGTH as usize]),
                ),
            ]))
            .await
            .expect("create/read response");

        let second_read = transport.recv_packet().await.expect("second read");
        let second_header = Smb2Header::decode(&second_read).expect("second read header");
        assert_eq!(second_header.command, Smb2Command::Read);
        assert_eq!(second_header.tree_id, 0x1122_3344);
        let second_body =
            Smb2ReadRequest::decode(&second_read[SMB2_HEADER_BYTES..]).expect("second read");
        assert_eq!(second_body.file_id, file_id);
        assert_eq!(second_body.offset, u64::from(DEFAULT_READ_CHUNK_LENGTH));
        assert_eq!(second_body.length, 4);
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Read, second_header.message_id),
                &read_response_body(b"TAIL"),
            ))
            .await
            .expect("second read response");

        let close = transport.recv_packet().await.expect("close request");
        let close_header = Smb2Header::decode(&close).expect("close header");
        assert_eq!(close_header.command, Smb2Command::Close);
        assert_eq!(close_header.tree_id, 0x1122_3344);
        let close_body = Smb2CloseRequest::decode(&close[SMB2_HEADER_BYTES..]).expect("close");
        assert_eq!(close_body.file_id, file_id);
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Close, close_header.message_id),
                &close_response_body(Smb2FileAttributes::ARCHIVE, eof),
            ))
            .await
            .expect("close response");
    }

    async fn handle_streamed_write_open<T>(transport: &mut DirectTcpTransport<T>)
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let packet = transport.recv_packet().await.expect("streamed OPEN");
        let header = Smb2Header::decode(&packet).unwrap();
        assert_eq!(header.command, Smb2Command::Create);
        let body = Smb2CreateRequest::decode(&packet[SMB2_HEADER_BYTES..]).unwrap();
        assert_eq!(body.name, "folder\\out.bin");
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Create, header.message_id),
                &create_response_body(fake_file_id(), Smb2FileAttributes::ARCHIVE, 0),
            ))
            .await
            .unwrap();
    }

    async fn handle_write_file<T>(transport: &mut DirectTcpTransport<T>)
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        handle_stat_missing(transport, "folder\\out.bin").await;

        let file_id = fake_file_id();
        let create = transport.recv_packet().await.expect("create write request");
        let create_header = Smb2Header::decode(&create).expect("create write header");
        assert_eq!(create_header.command, Smb2Command::Create);
        assert_eq!(create_header.tree_id, 0x1122_3344);
        let create_body =
            Smb2CreateRequest::decode(&create[SMB2_HEADER_BYTES..]).expect("create write");
        assert_eq!(create_body.name, "folder\\out.bin");
        assert_eq!(
            create_body.create_disposition,
            Smb2CreateDisposition::Create
        );
        assert!(create_body
            .create_options
            .contains(Smb2CreateOptions::NON_DIRECTORY_FILE));
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Create, create_header.message_id),
                &create_response_body(file_id, Smb2FileAttributes::ARCHIVE, 0),
            ))
            .await
            .expect("create write response");

        let write = transport.recv_packet().await.expect("write request");
        let write_header = Smb2Header::decode(&write).expect("write header");
        assert_eq!(write_header.command, Smb2Command::Write);
        assert_eq!(write_header.tree_id, 0x1122_3344);
        let write_body = Smb2WriteRequest::decode(&write[SMB2_HEADER_BYTES..]).expect("write");
        assert_eq!(write_body.file_id, file_id);
        assert_eq!(write_body.offset, 0);
        assert_eq!(write_body.data, b"native payload");
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Write, write_header.message_id),
                &write_response_body(write_body.data.len() as u32),
            ))
            .await
            .expect("write response");

        let close = transport.recv_packet().await.expect("close write request");
        let close_header = Smb2Header::decode(&close).expect("close write header");
        assert_eq!(close_header.command, Smb2Command::Close);
        assert_eq!(close_header.tree_id, 0x1122_3344);
        let close_body = Smb2CloseRequest::decode(&close[SMB2_HEADER_BYTES..]).expect("close");
        assert_eq!(close_body.file_id, file_id);
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Close, close_header.message_id),
                &close_response_body(Smb2FileAttributes::ARCHIVE, 14),
            ))
            .await
            .expect("close write response");
    }

    async fn handle_mkdir<T>(transport: &mut DirectTcpTransport<T>)
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let file_id = fake_file_id();
        let create = transport.recv_packet().await.expect("mkdir create request");
        let create_header = Smb2Header::decode(&create).expect("mkdir create header");
        assert_eq!(create_header.command, Smb2Command::Create);
        assert_eq!(create_header.tree_id, 0x1122_3344);
        let create_body =
            Smb2CreateRequest::decode(&create[SMB2_HEADER_BYTES..]).expect("mkdir create");
        assert_eq!(create_body.name, "folder\\new-dir");
        assert_eq!(
            create_body.create_disposition,
            Smb2CreateDisposition::Create
        );
        assert!(create_body
            .create_options
            .contains(Smb2CreateOptions::DIRECTORY_FILE));
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Create, create_header.message_id),
                &create_response_body(file_id, Smb2FileAttributes::DIRECTORY, 0),
            ))
            .await
            .expect("mkdir create response");

        let close = transport.recv_packet().await.expect("mkdir close request");
        let close_header = Smb2Header::decode(&close).expect("mkdir close header");
        assert_eq!(close_header.command, Smb2Command::Close);
        assert_eq!(close_header.tree_id, 0x1122_3344);
        let close_body = Smb2CloseRequest::decode(&close[SMB2_HEADER_BYTES..]).expect("close");
        assert_eq!(close_body.file_id, file_id);
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Close, close_header.message_id),
                &close_response_body(Smb2FileAttributes::DIRECTORY, 0),
            ))
            .await
            .expect("mkdir close response");
    }

    async fn handle_move_file<T>(transport: &mut DirectTcpTransport<T>)
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        handle_stat_found(
            transport,
            "",
            directory_entry_record(1, "file.bin", 32, 0x100, Smb2FileAttributes::ARCHIVE),
        )
        .await;
        handle_stat_missing(transport, "renamed.bin").await;

        let file_id = fake_file_id();
        let request = transport
            .recv_packet()
            .await
            .expect("rename compound request");
        let create_header = Smb2Header::decode(&request).expect("rename create header");
        let set_offset = create_header.next_command as usize;
        let set_info_header =
            Smb2Header::decode(&request[set_offset..]).expect("rename set info header");
        let close_offset = set_offset + set_info_header.next_command as usize;
        let close_header =
            Smb2Header::decode(&request[close_offset..]).expect("rename close header");
        assert_eq!(create_header.command, Smb2Command::Create);
        assert_eq!(set_info_header.command, Smb2Command::SetInfo);
        assert_eq!(close_header.command, Smb2Command::Close);
        let create_body = Smb2CreateRequest::decode(&request[SMB2_HEADER_BYTES..set_offset])
            .expect("rename create");
        assert_eq!(create_body.name, "file.bin");
        assert!(create_body
            .desired_access
            .contains(Smb2DesiredAccess::DELETE));
        let set_info_body =
            Smb2SetInfoRequest::decode(&request[set_offset + SMB2_HEADER_BYTES..close_offset])
                .expect("rename set info");
        assert_eq!(set_info_body.info_type, Smb2InfoType::File);
        assert_eq!(
            set_info_body.file_info_class,
            Smb2FileInfoClass::FILE_RENAME_INFORMATION
        );
        assert_eq!(set_info_body.file_id.persistent, u64::MAX);
        assert_eq!(set_info_body.file_id.volatile, u64::MAX);
        assert_eq!(set_info_body.buffer[0], 0);
        assert_eq!(
            u32::from_le_bytes([
                set_info_body.buffer[16],
                set_info_body.buffer[17],
                set_info_body.buffer[18],
                set_info_body.buffer[19]
            ]),
            encode_utf16le("renamed.bin").len() as u32
        );
        assert_eq!(
            &set_info_body.buffer[20..],
            encode_utf16le("renamed.bin").as_slice()
        );
        let close_body =
            Smb2CloseRequest::decode(&request[close_offset + SMB2_HEADER_BYTES..]).expect("close");
        assert_eq!(close_body.file_id.persistent, u64::MAX);
        assert_eq!(close_body.file_id.volatile, u64::MAX);
        transport
            .send_packet(&compound_response_packet(vec![
                (
                    Smb2Header::new(Smb2Command::Create, create_header.message_id),
                    create_response_body(file_id, Smb2FileAttributes::ARCHIVE, 32),
                ),
                (
                    Smb2Header::new(Smb2Command::SetInfo, set_info_header.message_id),
                    set_info_response_body(),
                ),
                (
                    Smb2Header::new(Smb2Command::Close, close_header.message_id),
                    close_response_body(Smb2FileAttributes::ARCHIVE, 32),
                ),
            ]))
            .await
            .expect("rename compound response");
    }

    async fn handle_delete_file<T>(transport: &mut DirectTcpTransport<T>)
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        handle_stat_found(
            transport,
            "",
            directory_entry_record(1, "file.bin", 32, 0x100, Smb2FileAttributes::ARCHIVE),
        )
        .await;

        let file_id = fake_file_id();
        let request = transport
            .recv_packet()
            .await
            .expect("delete compound request");
        let create_header = Smb2Header::decode(&request).expect("delete create header");
        let set_offset = create_header.next_command as usize;
        let set_info_header =
            Smb2Header::decode(&request[set_offset..]).expect("delete set info header");
        let close_offset = set_offset + set_info_header.next_command as usize;
        let close_header =
            Smb2Header::decode(&request[close_offset..]).expect("delete close header");
        let create_body = Smb2CreateRequest::decode(&request[SMB2_HEADER_BYTES..set_offset])
            .expect("delete create");
        assert_eq!(create_body.name, "file.bin");
        assert!(create_body
            .desired_access
            .contains(Smb2DesiredAccess::DELETE));
        assert!(create_body
            .create_options
            .contains(Smb2CreateOptions::NON_DIRECTORY_FILE));
        let set_info_body =
            Smb2SetInfoRequest::decode(&request[set_offset + SMB2_HEADER_BYTES..close_offset])
                .expect("delete set info");
        assert_eq!(set_info_body.info_type, Smb2InfoType::File);
        assert_eq!(
            set_info_body.file_info_class,
            Smb2FileInfoClass::FILE_DISPOSITION_INFORMATION
        );
        assert_eq!(set_info_body.file_id.persistent, u64::MAX);
        assert_eq!(set_info_body.file_id.volatile, u64::MAX);
        assert_eq!(set_info_body.buffer, vec![1]);
        let close_body =
            Smb2CloseRequest::decode(&request[close_offset + SMB2_HEADER_BYTES..]).expect("close");
        assert_eq!(close_body.file_id.persistent, u64::MAX);
        assert_eq!(close_body.file_id.volatile, u64::MAX);
        transport
            .send_packet(&compound_response_packet(vec![
                (
                    Smb2Header::new(Smb2Command::Create, create_header.message_id),
                    create_response_body(file_id, Smb2FileAttributes::ARCHIVE, 32),
                ),
                (
                    Smb2Header::new(Smb2Command::SetInfo, set_info_header.message_id),
                    set_info_response_body(),
                ),
                (
                    Smb2Header::new(Smb2Command::Close, close_header.message_id),
                    close_response_body(Smb2FileAttributes::ARCHIVE, 32),
                ),
            ]))
            .await
            .expect("delete compound response");
    }

    async fn handle_copy_file<T>(transport: &mut DirectTcpTransport<T>)
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let source_eof = u64::from(DEFAULT_COPYCHUNK_CHUNK_BYTES) + 7;
        let source_mtime = 116_444_736_020_000_000;
        handle_stat_found(
            transport,
            "",
            directory_entry_record(
                1,
                "file.bin",
                source_eof,
                0x100,
                Smb2FileAttributes::ARCHIVE,
            ),
        )
        .await;
        handle_stat_missing(transport, "copy.bin").await;

        let source_file_id = Smb2FileId {
            persistent: 0x1111_2222_3333_4444,
            volatile: 0x5555_6666_7777_8888,
        };
        let target_file_id = Smb2FileId {
            persistent: 0x9999_aaaa_bbbb_cccc,
            volatile: 0xdddd_eeee_ffff_0001,
        };
        let resume_key = [0x6b; SMB2_RESUME_KEY_BYTES];

        let source_create = transport
            .recv_packet()
            .await
            .expect("copy source create request");
        let source_create_header =
            Smb2Header::decode(&source_create).expect("copy source create header");
        assert_eq!(source_create_header.command, Smb2Command::Create);
        assert_eq!(source_create_header.tree_id, 0x1122_3344);
        let source_create_body = Smb2CreateRequest::decode(&source_create[SMB2_HEADER_BYTES..])
            .expect("copy source create");
        assert_eq!(source_create_body.name, "file.bin");
        assert!(source_create_body
            .desired_access
            .contains(Smb2DesiredAccess::GENERIC_READ));
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Create, source_create_header.message_id),
                &create_response_body(source_file_id, Smb2FileAttributes::ARCHIVE, source_eof),
            ))
            .await
            .expect("copy source create response");

        let target_create = transport
            .recv_packet()
            .await
            .expect("copy target create request");
        let target_create_header =
            Smb2Header::decode(&target_create).expect("copy target create header");
        assert_eq!(target_create_header.command, Smb2Command::Create);
        assert_eq!(target_create_header.tree_id, 0x1122_3344);
        let target_create_body = Smb2CreateRequest::decode(&target_create[SMB2_HEADER_BYTES..])
            .expect("copy target create");
        assert_eq!(target_create_body.name, "copy.bin");
        assert_eq!(
            target_create_body.create_disposition,
            Smb2CreateDisposition::Create
        );
        assert!(target_create_body
            .desired_access
            .contains(Smb2DesiredAccess::GENERIC_WRITE));
        assert!(target_create_body
            .create_options
            .contains(Smb2CreateOptions::NON_DIRECTORY_FILE));
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Create, target_create_header.message_id),
                &create_response_body(target_file_id, Smb2FileAttributes::ARCHIVE, 0),
            ))
            .await
            .expect("copy target create response");

        let resume = transport
            .recv_packet()
            .await
            .expect("resume key ioctl request");
        let resume_header = Smb2Header::decode(&resume).expect("resume key ioctl header");
        assert_eq!(resume_header.command, Smb2Command::Ioctl);
        assert_eq!(resume_header.tree_id, 0x1122_3344);
        let resume_body =
            Smb2IoctlRequest::decode(&resume[SMB2_HEADER_BYTES..]).expect("resume key ioctl");
        assert_eq!(resume_body.ctl_code, FSCTL_SRV_REQUEST_RESUME_KEY);
        assert_eq!(resume_body.file_id, source_file_id);
        assert!(resume_body.input.is_empty());
        assert_eq!(resume_body.max_output_response, 32);
        assert!(resume_body.flags.contains(Smb2IoctlFlags::IS_FSCTL));
        let mut resume_output = Vec::from(resume_key);
        resume_output.extend_from_slice(&0u32.to_le_bytes());
        resume_output.extend_from_slice(&0u32.to_le_bytes());
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Ioctl, resume_header.message_id),
                &ioctl_response_body(FSCTL_SRV_REQUEST_RESUME_KEY, source_file_id, &resume_output),
            ))
            .await
            .expect("resume key response");

        let copychunk = transport
            .recv_packet()
            .await
            .expect("copychunk ioctl request");
        let copychunk_header = Smb2Header::decode(&copychunk).expect("copychunk ioctl header");
        assert_eq!(copychunk_header.command, Smb2Command::Ioctl);
        assert_eq!(copychunk_header.tree_id, 0x1122_3344);
        let copychunk_body =
            Smb2IoctlRequest::decode(&copychunk[SMB2_HEADER_BYTES..]).expect("copychunk ioctl");
        assert_eq!(copychunk_body.ctl_code, FSCTL_SRV_COPYCHUNK);
        assert_eq!(copychunk_body.file_id, target_file_id);
        assert!(copychunk_body.flags.contains(Smb2IoctlFlags::IS_FSCTL));
        assert_eq!(&copychunk_body.input[..SMB2_RESUME_KEY_BYTES], &resume_key);
        assert_eq!(le_u32(&copychunk_body.input, 24), 2);
        assert_eq!(le_u64(&copychunk_body.input, 32), 0);
        assert_eq!(le_u64(&copychunk_body.input, 40), 0);
        assert_eq!(
            le_u32(&copychunk_body.input, 48),
            DEFAULT_COPYCHUNK_CHUNK_BYTES
        );
        assert_eq!(
            le_u64(&copychunk_body.input, 56),
            u64::from(DEFAULT_COPYCHUNK_CHUNK_BYTES)
        );
        assert_eq!(
            le_u64(&copychunk_body.input, 64),
            u64::from(DEFAULT_COPYCHUNK_CHUNK_BYTES)
        );
        assert_eq!(le_u32(&copychunk_body.input, 72), 7);
        let mut copychunk_output = vec![0; 12];
        put_u32(&mut copychunk_output, 0, 2);
        put_u32(&mut copychunk_output, 4, 7);
        put_u32(&mut copychunk_output, 8, source_eof as u32);
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Ioctl, copychunk_header.message_id),
                &ioctl_response_body(FSCTL_SRV_COPYCHUNK, target_file_id, &copychunk_output),
            ))
            .await
            .expect("copychunk response");

        for (label, file_id) in [
            ("copy source close request", source_file_id),
            ("copy target close request", target_file_id),
        ] {
            let close = transport.recv_packet().await.expect(label);
            let close_header = Smb2Header::decode(&close).expect("copy close header");
            assert_eq!(close_header.command, Smb2Command::Close);
            let close_body = Smb2CloseRequest::decode(&close[SMB2_HEADER_BYTES..]).expect("close");
            assert_eq!(close_body.file_id, file_id);
            transport
                .send_packet(&response_packet(
                    Smb2Header::new(Smb2Command::Close, close_header.message_id),
                    &close_response_body(Smb2FileAttributes::ARCHIVE, source_eof),
                ))
                .await
                .expect("copy close response");
        }

        let time_create = transport
            .recv_packet()
            .await
            .expect("copy time compound request");
        let time_create_header = Smb2Header::decode(&time_create).expect("copy time create header");
        let set_offset = time_create_header.next_command as usize;
        let set_info_header =
            Smb2Header::decode(&time_create[set_offset..]).expect("copy time set info header");
        let close_offset = set_offset + set_info_header.next_command as usize;
        let close_header =
            Smb2Header::decode(&time_create[close_offset..]).expect("copy time close header");
        assert_eq!(time_create_header.command, Smb2Command::Create);
        let time_create_body =
            Smb2CreateRequest::decode(&time_create[SMB2_HEADER_BYTES..set_offset])
                .expect("copy time create");
        assert_eq!(time_create_body.name, "copy.bin");
        assert!(time_create_body
            .desired_access
            .contains(Smb2DesiredAccess::FILE_WRITE_ATTRIBUTES));
        assert_eq!(set_info_header.command, Smb2Command::SetInfo);
        let set_info_body =
            Smb2SetInfoRequest::decode(&time_create[set_offset + SMB2_HEADER_BYTES..close_offset])
                .expect("copy time set info");
        assert_eq!(
            set_info_body.file_info_class,
            Smb2FileInfoClass::FILE_BASIC_INFORMATION
        );
        assert_eq!(set_info_body.file_id.persistent, u64::MAX);
        assert_eq!(set_info_body.file_id.volatile, u64::MAX);
        assert_eq!(le_u64(&set_info_body.buffer, 16), source_mtime);
        assert_eq!(close_header.command, Smb2Command::Close);
        let close_body = Smb2CloseRequest::decode(&time_create[close_offset + SMB2_HEADER_BYTES..])
            .expect("close");
        assert_eq!(close_body.file_id.persistent, u64::MAX);
        assert_eq!(close_body.file_id.volatile, u64::MAX);
        transport
            .send_packet(&compound_response_packet(vec![
                (
                    Smb2Header::new(Smb2Command::Create, time_create_header.message_id),
                    create_response_body(target_file_id, Smb2FileAttributes::ARCHIVE, source_eof),
                ),
                (
                    Smb2Header::new(Smb2Command::SetInfo, set_info_header.message_id),
                    set_info_response_body(),
                ),
                (
                    Smb2Header::new(Smb2Command::Close, close_header.message_id),
                    close_response_body(Smb2FileAttributes::ARCHIVE, source_eof),
                ),
            ]))
            .await
            .expect("copy time compound response");
    }

    async fn handle_stat_found<T>(
        transport: &mut DirectTcpTransport<T>,
        parent_path: &str,
        directory_record: Vec<u8>,
    ) where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let entry = iris_smb_client::decode_file_id_both_directory_entries(&directory_record)
            .unwrap()
            .remove(0);
        let path = if parent_path.is_empty() {
            entry.file_name
        } else {
            format!("{parent_path}\\{}", entry.file_name)
        };
        let request = transport.recv_packet().await.expect("stat create");
        let header = Smb2Header::decode(&request).unwrap();
        assert_eq!(header.command, Smb2Command::Create);
        let create = Smb2CreateRequest::decode(&request[SMB2_HEADER_BYTES..]).unwrap();
        assert_stat_request(&create, &path);
        let mut body =
            create_response_body(fake_file_id(), entry.file_attributes, entry.end_of_file);
        put_u64(&mut body, 8, entry.creation_time);
        put_u64(&mut body, 24, entry.last_write_time);
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Create, header.message_id),
                &body,
            ))
            .await
            .unwrap();
        let request = transport
            .recv_packet()
            .await
            .expect("stat close (not query directory)");
        let header = Smb2Header::decode(&request).unwrap();
        assert_eq!(header.command, Smb2Command::Close);
        assert_eq!(
            Smb2CloseRequest::decode(&request[SMB2_HEADER_BYTES..])
                .unwrap()
                .file_id,
            fake_file_id()
        );
        transport
            .send_packet(&response_packet(
                Smb2Header::new(Smb2Command::Close, header.message_id),
                &close_response_body(entry.file_attributes, entry.end_of_file),
            ))
            .await
            .unwrap();
    }

    fn assert_stat_request(create: &Smb2CreateRequest, path: &str) {
        assert_eq!(create.name, path);
        assert_eq!(
            create.desired_access,
            Smb2DesiredAccess::FILE_READ_ATTRIBUTES
        );
        assert_eq!(create.create_disposition, Smb2CreateDisposition::Open);
        assert_eq!(create.create_options, Smb2CreateOptions::default());
    }

    async fn handle_stat_missing<T>(transport: &mut DirectTcpTransport<T>, path: &str)
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let request = transport.recv_packet().await.expect("stat missing create");
        let header = Smb2Header::decode(&request).unwrap();
        assert_eq!(header.command, Smb2Command::Create);
        let create = Smb2CreateRequest::decode(&request[SMB2_HEADER_BYTES..]).unwrap();
        assert_stat_request(&create, path);
        let mut response = Smb2Header::new(Smb2Command::Create, header.message_id);
        response.status_or_channel_sequence = 0xc000_0034; // STATUS_OBJECT_NAME_NOT_FOUND
        transport
            .send_packet(&response_packet(response, &[]))
            .await
            .unwrap();
    }

    fn fake_file_id() -> Smb2FileId {
        Smb2FileId {
            persistent: 0x0102_0304_0506_0708,
            volatile: 0x1112_1314_1516_1718,
        }
    }

    fn ioctl_response_body(ctl_code: u32, file_id: Smb2FileId, output: &[u8]) -> Vec<u8> {
        iris_smb_client::smb2_ioctl_response_body(
            ctl_code,
            file_id,
            output,
            Smb2IoctlFlags::IS_FSCTL,
        )
        .expect("ioctl response body")
    }

    fn directory_entry_record(
        file_index: u32,
        file_name: &str,
        end_of_file: u64,
        file_id: u64,
        attributes: Smb2FileAttributes,
    ) -> Vec<u8> {
        let file_name_bytes = encode_utf16le(file_name);
        let mut body = vec![0; 104 + file_name_bytes.len()];
        put_u32(&mut body, 4, file_index);
        put_u64(&mut body, 8, 116_444_736_010_000_000);
        put_u64(&mut body, 24, 116_444_736_020_000_000);
        put_u64(&mut body, 40, end_of_file);
        put_u64(&mut body, 48, end_of_file);
        put_u32(&mut body, 56, attributes.0);
        put_u32(&mut body, 60, file_name_bytes.len() as u32);
        put_u64(&mut body, 96, file_id);
        body[104..].copy_from_slice(&file_name_bytes);
        body
    }
}
