use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::object::ObjectRef;
use iris_core_domain::types::progress::ByteRange;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// Remote reads are fetched in windows of this size so nearby header reads share one request.
const WINDOW_BYTES: usize = 256 * 1024;
const MAX_CACHED_WINDOWS: usize = 8;
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
}

impl BlockingBinaryReader for SourceFile {
    fn read_range(&self, offset: u64, len: usize) -> Option<Vec<u8>> {
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

    fn len(&self) -> u64 {
        self.len
    }
}

/// Most recently used windows, keyed by start offset.
#[derive(Default)]
struct WindowCache {
    order: VecDeque<u64>,
    windows: HashMap<u64, Arc<[u8]>>,
}

impl WindowCache {
    fn get(&mut self, start: u64) -> Option<Arc<[u8]>> {
        let bytes = self.windows.get(&start)?.clone();
        self.touch(start);
        Some(bytes)
    }

    fn insert(&mut self, start: u64, bytes: Arc<[u8]>) {
        if self.windows.insert(start, bytes).is_none() && self.order.len() >= MAX_CACHED_WINDOWS {
            if let Some(oldest) = self.order.pop_front() {
                self.windows.remove(&oldest);
            }
        }
        self.touch(start);
    }

    fn touch(&mut self, start: u64) {
        self.order.retain(|entry| *entry != start);
        self.order.push_back(start);
    }
}

/// Blocking reads of a remote object through its backend, for parsers that run on a blocking
/// thread. Small reads go through a window cache; the first failure is kept for `take_error`.
pub struct RemoteWindowSource<'a> {
    backend: &'a dyn BackendAdapter,
    object: &'a ObjectRef,
    len: u64,
    runtime_handle: tokio::runtime::Handle,
    cancel_token: Option<CancellationToken>,
    cache: Mutex<WindowCache>,
    first_error: Mutex<Option<io::Error>>,
}

impl<'a> RemoteWindowSource<'a> {
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
            cancel_token,
            cache: Mutex::new(WindowCache::default()),
            first_error: Mutex::new(None),
        }
    }

    /// The first read failure, if any.
    pub fn take_error(&self) -> Option<BackendError> {
        self.first_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .map(|error| BackendError::Internal(Box::new(error)))
    }

    pub fn read(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let result = self.read_unrecorded(offset, len);
        if let Err(error) = &result {
            let mut slot = self
                .first_error
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if slot.is_none() {
                *slot = Some(io::Error::new(error.kind(), error.to_string()));
            }
        }
        result
    }

    fn read_unrecorded(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let end = offset
            .checked_add(len as u64)
            .filter(|end| *end <= self.len)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "range exceeds source bounds for {}: {offset}+{len} > {}",
                        self.object.display_path, self.len
                    ),
                )
            })?;
        if len > WINDOW_BYTES {
            return self.fetch(offset, len);
        }

        let mut data = Vec::with_capacity(len);
        let mut cursor = offset;
        while cursor < end {
            let window_start = cursor - cursor % WINDOW_BYTES as u64;
            let window = self.window(window_start)?;
            let slice_start = (cursor - window_start) as usize;
            let slice_end = ((end - window_start) as usize).min(window.len());
            data.extend_from_slice(&window[slice_start..slice_end]);
            cursor = window_start + slice_end as u64;
        }
        Ok(data)
    }

    fn window(&self, start: u64) -> io::Result<Arc<[u8]>> {
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(start);
        if let Some(bytes) = cached {
            return Ok(bytes);
        }
        let len = ((start + WINDOW_BYTES as u64).min(self.len) - start) as usize;
        let bytes = Arc::<[u8]>::from(self.fetch(start, len)?);
        self.cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(start, bytes.clone());
        Ok(bytes)
    }

    fn fetch(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let path = &self.object.display_path;
        self.runtime_handle.block_on(async {
            ensure_not_cancelled(self.cancel_token.as_ref(), path)?;
            let mut reader = self
                .backend
                .read(
                    self.object,
                    Some(ByteRange {
                        offset,
                        length: Some(len as u64),
                    }),
                )
                .await
                .map_err(|error| io::Error::other(error.to_string()))?
                .stream;
            let mut buf = Vec::with_capacity(len);
            let mut chunk = vec![0u8; REMOTE_READ_CHUNK_BYTES.min(len.max(1))];
            while buf.len() < len {
                ensure_not_cancelled(self.cancel_token.as_ref(), path)?;
                let read_len = chunk.len().min(len - buf.len());
                let bytes_read = reader.read(&mut chunk[..read_len]).await?;
                if bytes_read == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!(
                            "short range read for {path}: requested {len}, got {}",
                            buf.len()
                        ),
                    ));
                }
                buf.extend_from_slice(&chunk[..bytes_read]);
            }
            Ok(buf)
        })
    }
}

impl BlockingBinaryReader for RemoteWindowSource<'_> {
    fn read_range(&self, offset: u64, len: usize) -> Option<Vec<u8>> {
        self.read(offset, len).ok()
    }

    fn len(&self) -> u64 {
        self.len
    }
}

fn ensure_not_cancelled(cancel_token: Option<&CancellationToken>, path: &str) -> io::Result<()> {
    if cancel_token.is_some_and(CancellationToken::is_cancelled) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            format!("remote range read canceled for {path}"),
        ))
    } else {
        Ok(())
    }
}

/// `Read + Seek` over a remote source, for decoders that expect a file.
pub struct RemoteRangeReader<'s, 'a> {
    source: &'s RemoteWindowSource<'a>,
    position: u64,
}

impl<'s, 'a> RemoteRangeReader<'s, 'a> {
    pub fn new(source: &'s RemoteWindowSource<'a>) -> Self {
        Self {
            source,
            position: 0,
        }
    }
}

impl Read for RemoteRangeReader<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.position >= self.source.len {
            return Ok(0);
        }
        let available = ((self.source.len - self.position) as usize).min(buf.len());
        let bytes = self.source.read(self.position, available)?;
        buf[..bytes.len()].copy_from_slice(&bytes);
        self.position += bytes.len() as u64;
        Ok(bytes.len())
    }
}

impl Seek for RemoteRangeReader<'_, '_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let (base, delta) = match pos {
            SeekFrom::Start(position) => {
                self.position = position;
                return Ok(position);
            }
            SeekFrom::Current(delta) => (self.position, delta),
            SeekFrom::End(delta) => (self.source.len, delta),
        };
        self.position = base.checked_add_signed(delta).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot seek before the start of the stream",
            )
        })?;
        Ok(self.position)
    }
}
