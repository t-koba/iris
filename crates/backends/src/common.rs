use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::list::ListCursor;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use tokio::fs;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, BufWriter};

pub fn parse_cursor_offset(cursor: Option<ListCursor>) -> Result<usize, BackendError> {
    match cursor {
        Some(c) => c.0.parse::<usize>().map_err(|e| {
            BackendError::Internal(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid list cursor '{}': {}", c.0, e),
            )))
        }),
        None => Ok(0),
    }
}

pub fn generate_conflict_name(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut counter = 1u32;
    loop {
        let candidate = path.with_file_name(format!("{} ({}){}", stem, counter, ext));
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

pub fn open_file_for_read(path: &Path, sequential: bool) -> Result<fs::File, BackendError> {
    let std_file = open_std_file_for_read(path, sequential)
        .map_err(|e| BackendError::Internal(Box::new(e)))?;
    apply_platform_read_hints(&std_file, sequential);
    Ok(fs::File::from_std(std_file))
}

pub async fn write_range_to_file(
    final_dst: &Path,
    reader: Pin<Box<dyn AsyncRead + Send>>,
    range: iris_core_domain::types::progress::ByteRange,
    final_size: u64,
) -> Result<(), BackendError> {
    const IO_BUFFER_SIZE: usize = 1024 * 1024;
    let expected_len = range
        .length
        .unwrap_or_else(|| final_size.saturating_sub(range.offset));
    let remaining_len = final_size.checked_sub(range.offset).ok_or_else(|| {
        BackendError::Internal(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "range write offset exceeds final file size",
        )))
    })?;
    if expected_len > remaining_len {
        return Err(BackendError::Internal(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "range write length exceeds final file size",
        ))));
    }

    if let Some(parent) = final_dst.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(final_dst)
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))?;
    file.set_len(final_size)
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))?;
    file.seek(std::io::SeekFrom::Start(range.offset))
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))?;

    let mut limited_reader = BufReader::with_capacity(IO_BUFFER_SIZE, reader.take(expected_len));
    let mut writer = BufWriter::with_capacity(IO_BUFFER_SIZE, file);
    let copied = tokio::io::copy_buf(&mut limited_reader, &mut writer)
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))?;
    writer
        .flush()
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))?;
    if copied != expected_len {
        return Err(BackendError::Internal(Box::new(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("range write copied {copied} bytes for expected {expected_len}"),
        ))));
    }

    Ok(())
}

pub async fn write_stream_to_file(
    final_dst: &Path,
    reader: Pin<Box<dyn AsyncRead + Send>>,
    content_length: Option<u64>,
) -> Result<(), BackendError> {
    // 1 MB buffers instead of the 8 KB default speed up both reads and writes.
    const IO_BUFFER_SIZE: usize = 1024 * 1024;

    if let Some(parent) = final_dst.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;
    }

    let std_file = create_file_for_write(final_dst, content_length)
        .map_err(|e| BackendError::Internal(Box::new(e)))?;
    let file = fs::File::from_std(std_file);
    let mut buf_reader = BufReader::with_capacity(IO_BUFFER_SIZE, reader);
    let mut writer = BufWriter::with_capacity(IO_BUFFER_SIZE, file);

    let copied = tokio::io::copy_buf(&mut buf_reader, &mut writer)
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))?;
    writer
        .flush()
        .await
        .map_err(|e| BackendError::Internal(Box::new(e)))?;

    // `create_file_for_write` preallocates when `content_length` is given. If the stream ends early,
    // the zeroed tail would remain and corrupt the destination, so truncate to the bytes written.
    if let Some(expected) = content_length {
        if expected > 0 && copied != expected {
            let inner = writer.into_inner();
            inner
                .set_len(copied)
                .await
                .map_err(|e| BackendError::Internal(Box::new(e)))?;
            inner
                .sync_all()
                .await
                .map_err(|e| BackendError::Internal(Box::new(e)))?;
        }
    }

    Ok(())
}

fn open_std_file_for_read(path: &Path, sequential: bool) -> Result<std::fs::File, std::io::Error> {
    let mut options = OpenOptions::new();
    options.read(true);

    #[cfg(not(windows))]
    let _ = sequential;

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
        if sequential {
            options.custom_flags(FILE_FLAG_SEQUENTIAL_SCAN);
        }
    }

    options.open(path)
}

fn create_file_for_write(
    path: &Path,
    content_length: Option<u64>,
) -> Result<std::fs::File, std::io::Error> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);

    // FILE_FLAG_SEQUENTIAL_SCAN only hints the read-side cache manager, so write-only handles skip it.

    let file = options.open(path)?;
    apply_platform_write_hints(&file);

    // Preallocate when the size is known to avoid fragmentation.
    if let Some(len) = content_length {
        if len > 0 {
            let _ = file.set_len(len);
        }
    }

    Ok(file)
}

fn apply_platform_read_hints(file: &std::fs::File, sequential: bool) {
    if !sequential {
        return;
    }

    #[cfg(target_os = "macos")]
    {
        const F_RDAHEAD: i32 = 45;
        const F_NOCACHE: i32 = 48;

        let _ = apply_macos_fcntl_hint(file, F_RDAHEAD, 1);
        let _ = apply_macos_fcntl_hint(file, F_NOCACHE, 1);
    }

    #[cfg(target_os = "linux")]
    {
        const POSIX_FADV_SEQUENTIAL: i32 = 2;
        const POSIX_FADV_WILLNEED: i32 = 3;

        let _ = apply_linux_fadvise(file, POSIX_FADV_SEQUENTIAL);
        let _ = apply_linux_fadvise(file, POSIX_FADV_WILLNEED);
    }
}

fn apply_platform_write_hints(file: &std::fs::File) {
    #[cfg(target_os = "macos")]
    {
        const F_NOCACHE: i32 = 48;
        let _ = apply_macos_fcntl_hint(file, F_NOCACHE, 1);
    }

    #[cfg(target_os = "linux")]
    {
        const POSIX_FADV_SEQUENTIAL: i32 = 2;
        let _ = apply_linux_fadvise(file, POSIX_FADV_SEQUENTIAL);
    }
}

#[cfg(target_os = "macos")]
fn apply_macos_fcntl_hint(
    file: &std::fs::File,
    cmd: i32,
    value: i32,
) -> Result<(), std::io::Error> {
    use std::os::fd::AsRawFd;

    unsafe extern "C" {
        fn fcntl(fd: std::os::raw::c_int, cmd: std::os::raw::c_int, ...) -> std::os::raw::c_int;
    }

    let result = unsafe { fcntl(file.as_raw_fd(), cmd, value) };
    if result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn apply_linux_fadvise(file: &std::fs::File, advice: i32) -> Result<(), std::io::Error> {
    use std::os::fd::AsRawFd;

    unsafe extern "C" {
        fn posix_fadvise(
            fd: std::os::raw::c_int,
            offset: i64,
            len: i64,
            advice: std::os::raw::c_int,
        ) -> std::os::raw::c_int;
    }

    let result = unsafe { posix_fadvise(file.as_raw_fd(), 0, 0, advice) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncRead;

    /// Regression: a `content_length` larger than the data must not leave zero padding behind.
    #[tokio::test]
    async fn write_stream_shorter_than_content_length_truncates_trailing_zeros() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dst = tmp.path().join("dst.bin");

        let payload: Vec<u8> = (0..500u32).map(|i| (i & 0xFF) as u8).collect();
        let actual_len = payload.len() as u64;
        let reader: Pin<Box<dyn AsyncRead + Send>> =
            Box::pin(std::io::Cursor::new(payload.clone()));

        write_stream_to_file(&dst, reader, Some(1000))
            .await
            .expect("write should succeed");

        let meta = std::fs::metadata(&dst).expect("stat dst");
        assert_eq!(
            meta.len(),
            actual_len,
            "destination must be truncated to actual bytes copied"
        );

        let readback = std::fs::read(&dst).expect("read dst");
        assert_eq!(readback, payload, "destination content must match source");
    }

    #[tokio::test]
    async fn write_stream_matching_content_length_preserves_data() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dst = tmp.path().join("dst.bin");

        let payload: Vec<u8> = (0..1024u32).map(|i| (i & 0xFF) as u8).collect();
        let len = payload.len() as u64;
        let reader: Pin<Box<dyn AsyncRead + Send>> =
            Box::pin(std::io::Cursor::new(payload.clone()));

        write_stream_to_file(&dst, reader, Some(len))
            .await
            .expect("write should succeed");

        let readback = std::fs::read(&dst).expect("read dst");
        assert_eq!(readback.len(), payload.len());
        assert_eq!(readback, payload);
    }

    #[tokio::test]
    async fn write_stream_without_content_length_writes_full_data() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dst = tmp.path().join("dst.bin");

        let payload: Vec<u8> = (0..2048u32).map(|i| ((i * 3) & 0xFF) as u8).collect();
        let reader: Pin<Box<dyn AsyncRead + Send>> =
            Box::pin(std::io::Cursor::new(payload.clone()));

        write_stream_to_file(&dst, reader, None)
            .await
            .expect("write should succeed");

        let readback = std::fs::read(&dst).expect("read dst");
        assert_eq!(readback, payload);
    }

    #[tokio::test]
    async fn write_stream_overwrite_clears_previous_tail() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dst = tmp.path().join("dst.bin");

        std::fs::write(&dst, vec![0xAAu8; 4096]).expect("seed file");

        let payload: Vec<u8> = (0..300u32).map(|_| 0x42u8).collect();
        let len = payload.len() as u64;
        let reader: Pin<Box<dyn AsyncRead + Send>> =
            Box::pin(std::io::Cursor::new(payload.clone()));

        write_stream_to_file(&dst, reader, Some(len))
            .await
            .expect("overwrite should succeed");

        let readback = std::fs::read(&dst).expect("read dst");
        assert_eq!(readback.len(), 300);
        assert!(readback.iter().all(|&b| b == 0x42));
    }

    #[tokio::test]
    async fn write_range_to_file_writes_at_offset_and_preserves_final_size() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dst = tmp.path().join("dst.bin");
        let reader: Pin<Box<dyn AsyncRead + Send>> =
            Box::pin(std::io::Cursor::new(b"range".to_vec()));

        write_range_to_file(
            &dst,
            reader,
            iris_core_domain::types::progress::ByteRange {
                offset: 4,
                length: Some(5),
            },
            12,
        )
        .await
        .expect("range write should succeed");

        let readback = std::fs::read(&dst).expect("read dst");
        assert_eq!(readback.len(), 12);
        assert_eq!(&readback[..4], &[0, 0, 0, 0]);
        assert_eq!(&readback[4..9], b"range");
        assert_eq!(&readback[9..], &[0, 0, 0]);
    }

    #[tokio::test]
    async fn write_range_to_file_rejects_range_beyond_final_size() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dst = tmp.path().join("dst.bin");
        let reader: Pin<Box<dyn AsyncRead + Send>> =
            Box::pin(std::io::Cursor::new(b"overflow".to_vec()));

        let result = write_range_to_file(
            &dst,
            reader,
            iris_core_domain::types::progress::ByteRange {
                offset: 8,
                length: Some(8),
            },
            12,
        )
        .await;

        assert!(result.is_err());
        assert!(!dst.exists());
    }
}
