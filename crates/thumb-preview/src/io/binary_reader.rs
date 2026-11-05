use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::object::ObjectRef;
use iris_core_domain::types::progress::ByteRange;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

pub const DEFAULT_REMOTE_READ_CACHE_WINDOW_BYTES: usize = 256 * 1024;
pub const DEFAULT_REMOTE_READ_CACHE_MAX_WINDOWS: usize = 8;
const REMOTE_READ_CHUNK_BYTES: usize = 64 * 1024;

pub trait BlockingBinaryReader {
    fn read_range(&self, offset: u64, len: usize) -> Option<Vec<u8>>;
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug)]
pub struct SourceFile {
    file: Mutex<File>,
    len: u64,
}

impl SourceFile {
    pub fn open(path: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        Some(Self {
            file: Mutex::new(file),
            len,
        })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn read_range(&self, offset: u64, len: usize) -> Option<Vec<u8>> {
        if len == 0 {
            return Some(Vec::new());
        }
        if offset.checked_add(len as u64)? > self.len {
            return None;
        }

        let mut file = self.file.lock().unwrap_or_else(|error| error.into_inner());
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = vec![0u8; len];
        file.read_exact(&mut buf).ok()?;
        Some(buf)
    }
}

impl BlockingBinaryReader for SourceFile {
    fn read_range(&self, offset: u64, len: usize) -> Option<Vec<u8>> {
        SourceFile::read_range(self, offset, len)
    }

    fn len(&self) -> u64 {
        self.len
    }
}

#[derive(Default)]
pub struct RangeReadCache {
    order: VecDeque<u64>,
    windows: HashMap<u64, Arc<[u8]>>,
}

impl RangeReadCache {
    pub fn get(&mut self, start: u64) -> Option<Arc<[u8]>> {
        let bytes = self.windows.get(&start)?.clone();
        if let Some(index) = self.order.iter().position(|entry| *entry == start) {
            self.order.remove(index);
        }
        self.order.push_back(start);
        Some(bytes)
    }

    pub fn insert(&mut self, start: u64, bytes: Arc<[u8]>) {
        if let std::collections::hash_map::Entry::Occupied(mut entry) = self.windows.entry(start) {
            entry.insert(bytes);
            if let Some(index) = self.order.iter().position(|existing| *existing == start) {
                self.order.remove(index);
            }
            self.order.push_back(start);
            return;
        }

        if self.order.len() >= DEFAULT_REMOTE_READ_CACHE_MAX_WINDOWS {
            if let Some(oldest) = self.order.pop_front() {
                self.windows.remove(&oldest);
            }
        }
        self.windows.insert(start, bytes);
        self.order.push_back(start);
    }
}

pub struct RemoteSource<'a> {
    backend: &'a dyn BackendAdapter,
    object: &'a ObjectRef,
    len: u64,
    cache: Mutex<RangeReadCache>,
    cancel_token: Option<CancellationToken>,
}

impl<'a> RemoteSource<'a> {
    pub fn new(backend: &'a dyn BackendAdapter, object: &'a ObjectRef, len: u64) -> Self {
        Self {
            backend,
            object,
            len,
            cache: Mutex::new(RangeReadCache::default()),
            cancel_token: None,
        }
    }

    pub fn with_cancellation(
        backend: &'a dyn BackendAdapter,
        object: &'a ObjectRef,
        len: u64,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            backend,
            object,
            len,
            cache: Mutex::new(RangeReadCache::default()),
            cancel_token: Some(cancel_token),
        }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub async fn read_range(
        &self,
        offset: u64,
        len: usize,
    ) -> Result<Option<Vec<u8>>, BackendError> {
        if len == 0 {
            return Ok(Some(Vec::new()));
        }
        let Some(end) = offset.checked_add(len as u64) else {
            return Ok(None);
        };
        if end > self.len {
            return Ok(None);
        }

        if len > DEFAULT_REMOTE_READ_CACHE_WINDOW_BYTES {
            return self.read_range_direct(offset, len).await.map(Some);
        }

        let mut data = Vec::with_capacity(len);
        let mut cursor = offset;
        while cursor < end {
            let window_start = (cursor / DEFAULT_REMOTE_READ_CACHE_WINDOW_BYTES as u64)
                * DEFAULT_REMOTE_READ_CACHE_WINDOW_BYTES as u64;
            let window_len = ((window_start + DEFAULT_REMOTE_READ_CACHE_WINDOW_BYTES as u64)
                .min(self.len)
                - window_start) as usize;
            let cached = self
                .cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(window_start);
            let window = match cached {
                Some(bytes) => bytes,
                None => {
                    let bytes =
                        Arc::<[u8]>::from(self.read_range_direct(window_start, window_len).await?);
                    self.cache
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .insert(window_start, bytes.clone());
                    bytes
                }
            };

            let slice_start = (cursor - window_start) as usize;
            let slice_end = ((end - window_start) as usize).min(window.len());
            data.extend_from_slice(&window[slice_start..slice_end]);
            cursor = window_start + slice_end as u64;
        }

        Ok(Some(data))
    }

    async fn read_range_direct(&self, offset: u64, len: usize) -> Result<Vec<u8>, BackendError> {
        if len == 0 {
            return Ok(Vec::new());
        }

        let handle = self
            .backend
            .read(
                self.object,
                Some(ByteRange {
                    offset,
                    length: Some(len as u64),
                }),
            )
            .await?;
        let mut reader = handle.stream;
        let buf = read_stream_exact_with_cancellation(
            &mut reader,
            len,
            self.cancel_token.as_ref(),
            &self.object.display_path,
        )
        .await
        .map_err(|error| BackendError::Internal(Box::new(error)))?;

        Ok(buf)
    }

    pub fn backend(&self) -> &'a dyn BackendAdapter {
        self.backend
    }

    pub fn object(&self) -> &'a ObjectRef {
        self.object
    }
}

pub struct BorrowedRemoteWindowSource<'a> {
    backend: &'a dyn BackendAdapter,
    object: &'a ObjectRef,
    len: u64,
    runtime_handle: tokio::runtime::Handle,
    cache: Mutex<RangeReadCache>,
    last_error: Mutex<Option<StoredReadError>>,
    cancel_token: Option<CancellationToken>,
}

#[derive(Debug)]
struct StoredReadError {
    kind: io::ErrorKind,
    message: String,
}

impl<'a> BorrowedRemoteWindowSource<'a> {
    pub fn new(
        backend: &'a dyn BackendAdapter,
        object: &'a ObjectRef,
        len: u64,
        runtime_handle: tokio::runtime::Handle,
        cancel_token: Option<CancellationToken>,
    ) -> Self {
        Self {
            backend,
            object,
            len,
            runtime_handle,
            cache: Mutex::new(RangeReadCache::default()),
            last_error: Mutex::new(None),
            cancel_token,
        }
    }

    pub fn take_error(&self) -> Option<BackendError> {
        self.last_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .map(|error| {
                BackendError::Internal(Box::new(io::Error::new(error.kind, error.message)))
            })
    }

    fn record_error(&self, error: &BackendError) {
        let mut slot = self
            .last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_none() {
            *slot = Some(StoredReadError {
                kind: io::ErrorKind::Other,
                message: error.to_string(),
            });
        }
    }

    fn record_io_error(&self, error: &io::Error) {
        let mut slot = self
            .last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_none() {
            *slot = Some(StoredReadError {
                kind: error.kind(),
                message: error.to_string(),
            });
        }
    }

    fn read_window(&self, start: u64) -> io::Result<Arc<[u8]>> {
        if let Some(bytes) = self
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(start)
        {
            return Ok(bytes);
        }

        let window_len = ((start + DEFAULT_REMOTE_READ_CACHE_WINDOW_BYTES as u64).min(self.len)
            - start) as usize;
        let bytes = Arc::<[u8]>::from(self.read_window_direct(start, window_len)?);
        self.cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(start, bytes.clone());
        Ok(bytes)
    }

    fn read_window_direct(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let result = self.runtime_handle.block_on(async {
            let handle = self
                .backend
                .read(
                    self.object,
                    Some(ByteRange {
                        offset,
                        length: Some(len as u64),
                    }),
                )
                .await
                .map_err(|error| {
                    self.record_error(&error);
                    backend_to_io_error(error)
                })?;
            let mut reader = handle.stream;
            read_stream_exact_with_cancellation(
                &mut reader,
                len,
                self.cancel_token.as_ref(),
                &self.object.display_path,
            )
            .await
        });
        if let Err(error) = &result {
            self.record_io_error(error);
        }
        result
    }
}

impl BlockingBinaryReader for BorrowedRemoteWindowSource<'_> {
    fn read_range(&self, offset: u64, len: usize) -> Option<Vec<u8>> {
        if len == 0 {
            return Some(Vec::new());
        }
        let end = offset.checked_add(len as u64)?;
        if end > self.len {
            return None;
        }

        let mut data = Vec::with_capacity(len);
        let mut cursor = offset;
        while cursor < end {
            let window_start = (cursor / DEFAULT_REMOTE_READ_CACHE_WINDOW_BYTES as u64)
                * DEFAULT_REMOTE_READ_CACHE_WINDOW_BYTES as u64;
            let window = self.read_window(window_start).ok()?;
            let slice_start = (cursor - window_start) as usize;
            let slice_end = ((end - window_start) as usize).min(window.len());
            data.extend_from_slice(&window[slice_start..slice_end]);
            cursor = window_start + slice_end as u64;
        }

        Some(data)
    }

    fn len(&self) -> u64 {
        self.len
    }
}

fn cancelled_read_error(path: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        format!("remote range read canceled for {}", path),
    )
}

fn ensure_not_cancelled(cancel_token: Option<&CancellationToken>, path: &str) -> io::Result<()> {
    if cancel_token.is_some_and(CancellationToken::is_cancelled) {
        Err(cancelled_read_error(path))
    } else {
        Ok(())
    }
}

async fn read_stream_exact_with_cancellation(
    reader: &mut (impl AsyncRead + Unpin),
    len: usize,
    cancel_token: Option<&CancellationToken>,
    path: &str,
) -> io::Result<Vec<u8>> {
    ensure_not_cancelled(cancel_token, path)?;
    let mut buf = Vec::with_capacity(len);
    let mut chunk = vec![0u8; REMOTE_READ_CHUNK_BYTES.min(len.max(1))];

    while buf.len() < len {
        ensure_not_cancelled(cancel_token, path)?;
        let remaining = len - buf.len();
        let read_len = chunk.len().min(remaining);
        let bytes_read = reader.read(&mut chunk[..read_len]).await?;
        if bytes_read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "short range read for {}: requested {}, got {}",
                    path,
                    len,
                    buf.len()
                ),
            ));
        }
        buf.extend_from_slice(&chunk[..bytes_read]);
    }

    ensure_not_cancelled(cancel_token, path)?;
    Ok(buf)
}

pub struct RemoteWindowSource {
    backend: Arc<dyn BackendAdapter>,
    object: ObjectRef,
    len: u64,
    runtime_handle: tokio::runtime::Handle,
    cache: Mutex<RangeReadCache>,
}

impl RemoteWindowSource {
    pub fn new(
        backend: Arc<dyn BackendAdapter>,
        object: ObjectRef,
        len: u64,
        runtime_handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            backend,
            object,
            len,
            runtime_handle,
            cache: Mutex::new(RangeReadCache::default()),
        }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn read_range(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let Some(end) = offset.checked_add(len as u64) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "range overflow while reading metadata",
            ));
        };
        if end > self.len {
            return Err(out_of_bounds_error(
                &self.object.display_path,
                offset,
                len,
                self.len,
            ));
        }

        let mut data = Vec::with_capacity(len);
        let mut cursor = offset;
        while cursor < end {
            let window_start = (cursor / DEFAULT_REMOTE_READ_CACHE_WINDOW_BYTES as u64)
                * DEFAULT_REMOTE_READ_CACHE_WINDOW_BYTES as u64;
            let window = self.read_window(window_start)?;
            let slice_start = (cursor - window_start) as usize;
            let slice_end = ((end - window_start) as usize).min(window.len());
            data.extend_from_slice(&window[slice_start..slice_end]);
            cursor = window_start + slice_end as u64;
        }

        Ok(data)
    }

    fn read_window(&self, start: u64) -> io::Result<Arc<[u8]>> {
        if let Some(bytes) = self
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(start)
        {
            return Ok(bytes);
        }

        let window_len = ((start + DEFAULT_REMOTE_READ_CACHE_WINDOW_BYTES as u64).min(self.len)
            - start) as usize;
        let bytes = Arc::<[u8]>::from(self.read_window_direct(start, window_len)?);
        self.cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(start, bytes.clone());
        Ok(bytes)
    }

    fn read_window_direct(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.runtime_handle.block_on(async {
            let handle = self
                .backend
                .read(
                    &self.object,
                    Some(ByteRange {
                        offset,
                        length: Some(len as u64),
                    }),
                )
                .await
                .map_err(backend_to_io_error)?;
            let mut reader = handle.stream;
            let mut buf = Vec::with_capacity(len);
            reader
                .read_to_end(&mut buf)
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            if buf.len() != len {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "short range read for {}: requested {}, got {}",
                        self.object.display_path,
                        len,
                        buf.len()
                    ),
                ));
            }
            Ok(buf)
        })
    }
}

pub struct RemoteRangeReader {
    source: Arc<RemoteWindowSource>,
    position: u64,
}

impl RemoteRangeReader {
    pub fn new(source: Arc<RemoteWindowSource>) -> Self {
        Self {
            source,
            position: 0,
        }
    }
}

impl Read for RemoteRangeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.position >= self.source.len() {
            return Ok(0);
        }

        let available = ((self.source.len() - self.position) as usize).min(buf.len());
        let bytes = self.source.read_range(self.position, available)?;
        buf[..bytes.len()].copy_from_slice(&bytes);
        self.position += bytes.len() as u64;
        Ok(bytes.len())
    }
}

impl Seek for RemoteRangeReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let next = match pos {
            SeekFrom::Start(position) => position,
            SeekFrom::Current(delta) => {
                let current = self.position as i128;
                let next = current + delta as i128;
                if next < 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "cannot seek before start of metadata stream",
                    ));
                }
                next as u64
            }
            SeekFrom::End(delta) => {
                let end = self.source.len() as i128;
                let next = end + delta as i128;
                if next < 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "cannot seek before start of metadata stream",
                    ));
                }
                next as u64
            }
        };
        self.position = next;
        Ok(self.position)
    }
}

pub fn backend_to_io_error(error: BackendError) -> io::Error {
    io::Error::other(error.to_string())
}

pub fn join_to_backend_error(error: tokio::task::JoinError) -> BackendError {
    BackendError::Internal(Box::new(io::Error::other(format!(
        "metadata extraction task failed: {}",
        error
    ))))
}

fn out_of_bounds_error(label: &str, offset: u64, len: usize, source_len: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        format!(
            "range exceeds source bounds for {}: {}..{} > {}",
            label,
            offset,
            offset.saturating_add(len as u64),
            source_len
        ),
    )
}
