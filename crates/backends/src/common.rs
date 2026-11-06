use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::list::ListCursor;
use std::fs::OpenOptions;
use std::path::Path;
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
    if let Some(parent) = final_dst.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| BackendError::Internal(Box::new(e)))?;
    }

    let std_file = create_file_for_write(final_dst, content_length)
        .map_err(|e| BackendError::Internal(Box::new(e)))?;
    write_stream_to_open_file(std_file, reader, content_length).await
}

/// The caller owns the path's lifetime; this function never recreates it on a blocking worker.
pub(crate) async fn write_stream_to_open_file(
    std_file: std::fs::File,
    reader: Pin<Box<dyn AsyncRead + Send>>,
    content_length: Option<u64>,
) -> Result<(), BackendError> {
    // 1 MB buffers instead of the 8 KB default speed up both reads and writes.
    const IO_BUFFER_SIZE: usize = 1024 * 1024;
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
        if copied != expected {
            let inner = writer.into_inner();
            inner
                .set_len(copied)
                .await
                .map_err(|e| BackendError::Internal(Box::new(e)))?;
            inner
                .sync_all()
                .await
                .map_err(|e| BackendError::Internal(Box::new(e)))?;
            return Err(BackendError::Internal(Box::new(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("stream write copied {copied} bytes for expected {expected}"),
            ))));
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

pub(crate) fn create_file_for_write(
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

/// Rename without replacing any destination entry, including dangling symlinks.
///
/// Maps to the operating system no-replace primitive where available (Linux
/// renameat2 with NOREPLACE, macOS renamex with EXCL, Windows MoveFileEx
/// without REPLACE). The final install is the authority: callers must never
/// check existence and then rename.
///
/// All occupied destinations (regular files, directories and dangling links)
/// fail with [`std::io::ErrorKind::AlreadyExists`]. Unsupported filesystems
/// or platforms fail closed with an error instead of replacing. Mounted
/// filesystems must implement the primitive correctly; this is not a
/// guarantee of atomicity or crash recovery on arbitrary NFS servers.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn rename_no_replace(src: &Path, dst: &Path) -> std::io::Result<()> {
    match rustix::fs::renameat_with(
        rustix::fs::CWD,
        src,
        rustix::fs::CWD,
        dst,
        rustix::fs::RenameFlags::NOREPLACE,
    ) {
        Ok(()) => Ok(()),
        Err(err) => Err(occupied_error(dst, std::io::Error::from(err))),
    }
}

/// Rename without replacing any destination entry, including dangling symlinks.
///
/// Maps to the operating system no-replace primitive where available (Linux
/// renameat2 with NOREPLACE, macOS renamex with EXCL, Windows MoveFileEx
/// without REPLACE). The final install is the authority: callers must never
/// check existence and then rename.
///
/// All occupied destinations (regular files, directories and dangling links)
/// fail with [`std::io::ErrorKind::AlreadyExists`]. Unsupported filesystems
/// or platforms fail closed with an error instead of replacing. Mounted
/// filesystems must implement the primitive correctly; this is not a
/// guarantee of atomicity or crash recovery on arbitrary NFS servers.
#[cfg(windows)]
fn windows_verbatim_path(path: &Path, units: &[u16]) -> Option<Vec<u16>> {
    use std::path::{Component, Prefix};

    let prefix = match path.components().next()? {
        Component::Prefix(prefix) => prefix,
        _ => return None,
    };
    let (mut verbatim, suffix) = match prefix.kind() {
        Prefix::Disk(_) => (r"\\?\".encode_utf16().collect::<Vec<_>>(), units),
        Prefix::UNC(_, _) if units.len() >= 2 => {
            (r"\\?\UNC\".encode_utf16().collect::<Vec<_>>(), &units[2..])
        }
        _ => return None,
    };
    verbatim.extend(suffix.iter().map(|&unit| {
        if unit == b'/' as u16 {
            b'\\' as u16
        } else {
            unit
        }
    }));
    Some(verbatim)
}

#[cfg(windows)]
fn windows_has_dot_segments(units: &[u16]) -> bool {
    const DOT: u16 = b'.' as u16;
    units
        .split(|unit| *unit == b'\\' as u16 || *unit == b'/' as u16)
        .any(|component| {
            (component.len() == 1 && component[0] == DOT)
                || (component.len() == 2 && component[0] == DOT && component[1] == DOT)
        })
}

#[cfg(windows)]
fn windows_path_to_wide(path: &Path) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    const MAX_PATH: usize = 260;

    let input: Vec<u16> = path.as_os_str().encode_wide().collect();
    if input.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "NUL in path",
        ));
    }

    // Prefix long absolute paths before GetFullPathNameW can apply MAX_PATH.
    let absolute_input = if path.is_absolute() && input.len() >= MAX_PATH {
        if windows_has_dot_segments(&input) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "long Windows paths with dot segments are unsupported",
            ));
        }
        windows_verbatim_path(path, &input)
            .map(|units| std::path::PathBuf::from(std::ffi::OsString::from_wide(&units)))
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    };

    let path = std::path::absolute(absolute_input)?;
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "NUL in absolute path",
        ));
    }
    if wide.len() >= MAX_PATH {
        if windows_has_dot_segments(&wide) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "long Windows paths with dot segments are unsupported",
            ));
        }
        if let Some(verbatim) = windows_verbatim_path(&path, &wide) {
            wide = verbatim;
        }
    }
    if wide.len() >= 32_767 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows path exceeds the extended-length limit",
        ));
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(windows)]
pub fn rename_no_replace(src: &Path, dst: &Path) -> std::io::Result<()> {
    let from = windows_path_to_wide(src)?;
    let to = windows_path_to_wide(dst)?;
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
    };
    if ok != 0 {
        Ok(())
    } else {
        Err(occupied_error(dst, std::io::Error::last_os_error()))
    }
}

/// Rename without replacing any destination entry, including dangling symlinks.
///
/// Maps to the operating system no-replace primitive where available (Linux
/// renameat2 with NOREPLACE, macOS renamex with EXCL, Windows MoveFileEx
/// without REPLACE). The final install is the authority: callers must never
/// check existence and then rename.
///
/// All occupied destinations (regular files, directories and dangling links)
/// fail with [`std::io::ErrorKind::AlreadyExists`]. Unsupported filesystems
/// or platforms fail closed with an error instead of replacing. Mounted
/// filesystems must implement the primitive correctly; this is not a
/// guarantee of atomicity or crash recovery on arbitrary NFS servers.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn rename_no_replace(_src: &Path, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no-replace rename is not supported on this platform",
    ))
}

fn occupied_error(dst: &Path, err: std::io::Error) -> std::io::Error {
    if err.kind() == std::io::ErrorKind::AlreadyExists {
        return err;
    }
    if std::fs::symlink_metadata(dst).is_ok() {
        return std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("destination exists: {}", dst.display()),
        );
    }
    err
}

fn apply_platform_read_hints(_file: &std::fs::File, sequential: bool) {
    if !sequential {
        return;
    }

    #[cfg(target_os = "macos")]
    {
        const F_RDAHEAD: i32 = 45;
        const F_NOCACHE: i32 = 48;

        let _ = apply_macos_fcntl_hint(_file, F_RDAHEAD, 1);
        let _ = apply_macos_fcntl_hint(_file, F_NOCACHE, 1);
    }

    #[cfg(target_os = "linux")]
    {
        let _ = apply_linux_fadvise(_file, rustix::fs::Advice::Sequential);
        let _ = apply_linux_fadvise(_file, rustix::fs::Advice::WillNeed);
    }
}

fn apply_platform_write_hints(_file: &std::fs::File) {
    #[cfg(target_os = "macos")]
    {
        const F_NOCACHE: i32 = 48;
        let _ = apply_macos_fcntl_hint(_file, F_NOCACHE, 1);
    }

    #[cfg(target_os = "linux")]
    {
        let _ = apply_linux_fadvise(_file, rustix::fs::Advice::Sequential);
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
fn apply_linux_fadvise(
    file: &std::fs::File,
    advice: rustix::fs::Advice,
) -> Result<(), std::io::Error> {
    rustix::fs::fadvise(file, 0, None, advice).map_err(std::io::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncRead;

    /// A short stream must fail without leaving preallocated zero padding behind.
    #[tokio::test]
    async fn write_stream_shorter_than_content_length_fails_without_trailing_zeros() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dst = tmp.path().join("dst.bin");

        let payload: Vec<u8> = (0..500u32).map(|i| (i & 0xFF) as u8).collect();
        let actual_len = payload.len() as u64;
        let reader: Pin<Box<dyn AsyncRead + Send>> =
            Box::pin(std::io::Cursor::new(payload.clone()));

        let error = write_stream_to_file(&dst, reader, Some(1000))
            .await
            .expect_err("short stream must fail");
        let BackendError::Internal(error) = error else {
            panic!("expected an I/O error");
        };
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::UnexpectedEof
        );

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
    async fn write_stream_with_zero_content_length_rejects_nonempty_data() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dst = tmp.path().join("dst.bin");
        let reader = Box::pin(std::io::Cursor::new(b"extra"));

        let error = write_stream_to_file(&dst, reader, Some(0))
            .await
            .expect_err("nonempty stream with zero content length must fail");
        let BackendError::Internal(error) = error else {
            panic!("expected an I/O error");
        };
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        assert_eq!(std::fs::read(&dst).expect("read dst"), b"extra");
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

    #[test]
    fn no_replace_preserves_occupied_entries_and_moves_to_absent_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        std::fs::write(&src, b"source").expect("source");
        std::fs::write(&dst, b"destination").expect("destination");
        assert_eq!(
            rename_no_replace(&src, &dst).expect_err("occupied").kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&dst).expect("destination"), b"destination");
        std::fs::remove_file(&dst).expect("remove destination");
        std::fs::create_dir(&dst).expect("directory");
        assert_eq!(
            rename_no_replace(&src, &dst)
                .expect_err("directory occupied")
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert!(dst.is_dir());
        std::fs::remove_dir(&dst).expect("remove directory");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("missing", &dst).expect("dangling");
            assert_eq!(
                rename_no_replace(&src, &dst)
                    .expect_err("dangling occupied")
                    .kind(),
                std::io::ErrorKind::AlreadyExists
            );
            assert_eq!(
                std::fs::read_link(&dst).expect("link"),
                Path::new("missing")
            );
            std::fs::remove_file(&dst).expect("remove link");
        }
        rename_no_replace(&src, &dst).expect("absent destination");
        assert!(!src.exists());
        assert_eq!(std::fs::read(&dst).expect("installed"), b"source");
    }

    #[cfg(windows)]
    #[test]
    fn no_replace_rejects_embedded_nul_before_mutating_a_truncated_path() {
        use std::os::windows::ffi::OsStringExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("source");
        let truncated_dst = temp.path().join("dst");
        std::fs::write(&src, b"source").expect("source");
        let mut invalid_name: Vec<u16> = "dst".encode_utf16().collect();
        invalid_name.push(0);
        invalid_name.extend("suffix".encode_utf16());
        let invalid_dst = temp
            .path()
            .join(std::ffi::OsString::from_wide(&invalid_name));

        let error = rename_no_replace(&src, &invalid_dst).expect_err("NUL path must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(src.exists());
        assert!(!truncated_dst.exists());
    }

    #[cfg(windows)]
    #[test]
    fn long_absolute_disk_and_unc_paths_get_verbatim_prefixes() {
        let long_disk = std::path::PathBuf::from("C:/")
            .join("a".repeat(180))
            .join("b".repeat(100));
        let disk = windows_path_to_wide(&long_disk).expect("long disk path");
        let disk_path = String::from_utf16(&disk[..disk.len() - 1]).expect("disk UTF-16");
        assert!(disk_path.starts_with(r"\\?\C:\"));
        assert!(!disk_path.contains('/'));
        assert_eq!(disk.last(), Some(&0));

        let long_unc = std::path::PathBuf::from("//server/share")
            .join("a".repeat(180))
            .join("b".repeat(100));
        let unc = windows_path_to_wide(&long_unc).expect("long UNC path");
        let unc_path = String::from_utf16(&unc[..unc.len() - 1]).expect("UNC UTF-16");
        assert!(unc_path.starts_with(r"\\?\UNC\server\share\"));
        assert!(!unc_path.contains('/'));
        assert_eq!(unc.last(), Some(&0));

        let long_dot_segment =
            std::path::PathBuf::from(format!("C:/{}/../{}", "a".repeat(180), "b".repeat(100)));
        assert_eq!(
            windows_path_to_wide(&long_dot_segment)
                .expect_err("long dot segments must fail closed")
                .kind(),
            std::io::ErrorKind::Unsupported
        );
    }
}
