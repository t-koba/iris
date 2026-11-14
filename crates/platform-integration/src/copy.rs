use crate::file_times::preserve_modified_time_of;
use std::{io, path::Path};

/// Route large macOS and Windows copies through the existing streaming fallback.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const PLATFORM_COPY_SIZE_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PlatformCopyError {
    #[error("platform copy intentionally skipped: {0}")]
    Unsupported(&'static str),

    #[error("platform copy I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl PlatformCopyError {
    pub fn should_fallback_to_streaming(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }
}

fn same_file_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "source and destination refer to the same file",
    )
}

/// Reject copies whose existing destination resolves to the source file, including
/// hard links and symlinks. Identity lookup errors other than a missing destination
/// fail closed; return whether a distinct destination existed. Copy paths also compare
/// the actual opened handles before truncation.
pub fn ensure_distinct_files(src: &Path, dst: &Path) -> io::Result<bool> {
    let destination = match file_identity(dst) {
        Ok(identity) => identity,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if file_identity(src)? == destination {
        return Err(same_file_error());
    }
    Ok(true)
}

#[cfg(unix)]
fn file_identity(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_identity(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
    };
    // Query identity without requiring read access to the destination's contents.
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let handle = winapi_util::Handle::from_file(file);
    let information = winapi_util::file::information(&handle)?;
    Ok((information.volume_serial_number(), information.file_index()))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_path: &Path) -> io::Result<(u64, u64)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "filesystem identity is unavailable on this platform",
    ))
}

#[cfg(unix)]
fn handle_identity(file: &std::fs::File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn handle_identity(file: &std::fs::File) -> io::Result<(u64, u64)> {
    use std::os::windows::fs::MetadataExt;
    let metadata = file.metadata()?;
    let volume = metadata.volume_serial_number().ok_or_else(|| {
        io::Error::new(io::ErrorKind::Unsupported, "volume identity is unavailable")
    })?;
    let index = metadata.file_index().ok_or_else(|| {
        io::Error::new(io::ErrorKind::Unsupported, "file identity is unavailable")
    })?;
    Ok((u64::from(volume), index))
}

#[cfg(not(any(unix, windows)))]
fn handle_identity(_file: &std::fs::File) -> io::Result<(u64, u64)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "filesystem identity is unavailable on this platform",
    ))
}

/// Copy a file using handle-checked `std::io::copy` on a blocking worker.
///
/// The opened destination is compared with the opened source before truncation;
/// this also applies to mounted SMB/NFS paths. On Unix, new destinations are
/// restricted to at most mode 0600 before writing; source permissions are applied
/// after copying succeeds.
pub async fn platform_copy(
    src: impl AsRef<Path>,
    dst: impl AsRef<Path>,
) -> Result<u64, PlatformCopyError> {
    let src = src.as_ref().to_path_buf();
    let dst = dst.as_ref().to_path_buf();
    tokio::task::spawn_blocking(move || platform_copy_blocking(&src, &dst))
        .await
        .map_err(|error| std::io::Error::other(format!("platform copy task failed: {error}")))?
}

/// Copy a regular file through `std::io::copy(File, File)` on the blocking pool.
///
/// This fallback preserves permissions of existing destinations. On Unix, new
/// destinations are restricted to at most mode 0600 before writing and receive
/// their default creation permissions (filtered by umask) after a successful copy.
pub async fn stream_file_copy(
    src: impl AsRef<Path>,
    dst: impl AsRef<Path>,
) -> Result<u64, PlatformCopyError> {
    let src = src.as_ref().to_path_buf();
    let dst = dst.as_ref().to_path_buf();
    tokio::task::spawn_blocking(move || stream_file_copy_blocking(&src, &dst))
        .await
        .map_err(|error| std::io::Error::other(format!("stream copy task failed: {error}")))?
}

fn copy_open_handles(
    src: &Path,
    dst: &Path,
    copy_permissions: bool,
) -> Result<u64, PlatformCopyError> {
    let metadata = std::fs::metadata(src)?;
    if !metadata.is_file() {
        return Err(PlatformCopyError::Unsupported(
            "source is not a regular file",
        ));
    }
    ensure_distinct_files(src, dst)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }

    copy_open_handles_after_preflight(src, dst, copy_permissions, std::io::copy)
}

/// Copy using handles after the path preflight and parent creation have succeeded.
fn copy_open_handles_after_preflight(
    src: &Path,
    dst: &Path,
    copy_permissions: bool,
    copy: impl FnOnce(&mut std::fs::File, &mut std::fs::File) -> io::Result<u64>,
) -> Result<u64, PlatformCopyError> {
    let mut src_file = std::fs::File::open(src)?;
    let src_metadata = src_file.metadata()?;
    if !src_metadata.is_file() {
        return Err(PlatformCopyError::Unsupported(
            "source is not a regular file",
        ));
    }
    let (mut dst_file, destination_created) = open_destination(dst)?;

    // The handles, not the earlier paths, are authoritative. Truncation and
    // copying use this exact destination handle, so later path replacement
    // cannot redirect writes into the source.
    if handle_identity(&src_file)? == handle_identity(&dst_file)? {
        return Err(PlatformCopyError::Io(same_file_error()));
    }
    // Observe the kernel's creation mode rather than reading/changing the
    // process-global umask. Only an exclusively created, still-empty file may
    // have its permissions restricted here; existing files keep their mode.
    let creation_permissions = if cfg!(unix) && destination_created {
        let permissions = dst_file.metadata()?.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            dst_file
                .set_permissions(std::fs::Permissions::from_mode(permissions.mode() & 0o600))?;
        }
        Some(permissions)
    } else {
        None
    };
    dst_file.set_len(0)?;
    let copied = copy(&mut src_file, &mut dst_file)?;
    if copy_permissions {
        dst_file.set_permissions(src_metadata.permissions())?;
    } else if let Some(permissions) = creation_permissions {
        dst_file.set_permissions(permissions)?;
    }
    if let Ok(mtime) = src_metadata.modified() {
        preserve_modified_time_of(&dst_file, mtime)?;
    }
    Ok(copied)
}

/// Open without truncation and determine creation atomically. `create_new`
/// refuses symlinks, so follow a dangling link only after ordinary opening found
/// no target. A race that wins both open attempts fails closed rather than spinning;
/// writes and permission changes wait for the caller's handle-identity check.
fn open_destination(dst: &Path) -> io::Result<(std::fs::File, bool)> {
    let open_existing = || std::fs::OpenOptions::new().write(true).open(dst);
    match open_existing() {
        Ok(file) => return Ok((file, false)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    // The default creation mode is 0666, filtered by the process umask.
    // The caller restricts the empty file before writing any content.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
    {
        Ok(file) => Ok((file, true)),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => match open_existing() {
            Ok(file) => Ok((file, false)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let metadata = std::fs::symlink_metadata(dst)?;
                if !metadata.is_symlink() {
                    return Err(error);
                }
                let target = std::fs::read_link(dst)?;
                let target = if target.is_absolute() {
                    target
                } else {
                    dst.parent().unwrap_or_else(|| Path::new("")).join(target)
                };
                open_destination(&target)
            }
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    }
}

/// Blocking form of [`platform_copy`]; callers must use a blocking worker.
pub fn platform_copy_blocking(src: &Path, dst: &Path) -> Result<u64, PlatformCopyError> {
    let metadata = std::fs::metadata(src)?;
    if !metadata.is_file() {
        return Err(PlatformCopyError::Unsupported(
            "source is not a regular file",
        ));
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if metadata.len() > PLATFORM_COPY_SIZE_LIMIT {
        return Err(PlatformCopyError::Unsupported(
            "large copy uses the streaming fallback",
        ));
    }

    copy_open_handles(src, dst, true)
}

/// Blocking form of [`stream_file_copy`]; callers must use a blocking worker.
pub fn stream_file_copy_blocking(src: &Path, dst: &Path) -> Result<u64, PlatformCopyError> {
    copy_open_handles(src, dst, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_times::preserve_modified_time;

    #[test]
    fn copy_helpers_reject_same_file_without_changing_bytes() {
        for copy in [platform_copy_blocking, stream_file_copy_blocking] {
            let temp = tempfile::tempdir().expect("tempdir");
            let src = temp.path().join("src");
            let alias = temp.path().join("hard-link");
            std::fs::write(&src, b"keep original bytes").expect("source");
            std::fs::hard_link(&src, &alias).expect("hard link");
            let destinations = vec![src.clone(), alias];
            #[cfg(unix)]
            let destinations = {
                let mut destinations = destinations;
                let link = temp.path().join("symlink");
                std::os::unix::fs::symlink(&src, &link).expect("symlink");
                destinations.push(link);
                destinations
            };
            for dst in destinations {
                let error = copy(&src, &dst).expect_err("same file must fail");
                assert!(!error.should_fallback_to_streaming());
                assert!(matches!(error, PlatformCopyError::Io(ref e)
                    if e.kind() == io::ErrorKind::InvalidInput));
                assert_eq!(std::fs::read(&src).unwrap(), b"keep original bytes");
                assert_eq!(std::fs::read(&dst).unwrap(), b"keep original bytes");
            }
        }
    }

    #[test]
    fn opened_handle_check_rejects_alias_created_after_path_preflight() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        std::fs::write(&src, b"keep original bytes").expect("source");
        std::fs::write(&dst, b"distinct destination").expect("destination");

        let destination_existed =
            ensure_distinct_files(&src, &dst).expect("path preflight sees distinct files");
        assert!(destination_existed);
        // Replace the target after path preflight but before the actual handles
        // are opened, then exercise the exact handle-copy path used by both APIs.
        std::fs::remove_file(&dst).expect("remove original destination");
        std::fs::hard_link(&src, &dst).expect("create concurrent alias");
        let error = copy_open_handles_after_preflight(&src, &dst, false, std::io::copy)
            .expect_err("the compared handles must reject the new alias");

        assert!(matches!(error, PlatformCopyError::Io(ref error)
            if error.kind() == io::ErrorKind::InvalidInput));
        assert_eq!(std::fs::read(&src).unwrap(), b"keep original bytes");
        assert_eq!(std::fs::read(&dst).unwrap(), b"keep original bytes");
    }

    #[tokio::test]
    async fn platform_copy_preserves_bytes_and_mtime() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("src.bin");
        let dst = temp.path().join("nested").join("dst.bin");
        std::fs::write(&src, b"hello").expect("write src");

        let old_mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(12345);
        preserve_modified_time(&src, old_mtime).expect("set mtime");

        std::fs::create_dir_all(dst.parent().unwrap()).expect("destination parent");
        std::fs::write(&dst, b"old destination is longer").expect("old destination");
        let copied = platform_copy(&src, &dst).await.expect("platform copy");
        assert_eq!(copied, 5);
        assert_eq!(std::fs::read(&dst).expect("read dst"), b"hello");
        assert_eq!(
            std::fs::metadata(&dst)
                .expect("dst metadata")
                .modified()
                .expect("dst mtime"),
            old_mtime
        );
    }

    #[tokio::test]
    async fn platform_copy_rejects_directories_for_streaming_fallback() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dst = temp.path().join("dst");
        let error = platform_copy(temp.path(), &dst)
            .await
            .expect_err("directory should not be copied by platform_copy");
        assert!(error.should_fallback_to_streaming());
    }

    #[tokio::test]
    async fn stream_file_copy_preserves_bytes_and_mtime() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("src.bin");
        let dst = temp.path().join("nested").join("dst.bin");
        let bytes = (0..8192u32)
            .map(|value| (value % 251) as u8)
            .collect::<Vec<_>>();
        std::fs::write(&src, &bytes).expect("write src");

        let old_mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(54321);
        preserve_modified_time(&src, old_mtime).expect("set mtime");

        std::fs::create_dir_all(dst.parent().unwrap()).expect("destination parent");
        std::fs::write(&dst, vec![0; bytes.len() + 1]).expect("old destination");
        let copied = stream_file_copy(&src, &dst).await.expect("stream copy");
        assert_eq!(copied, bytes.len() as u64);
        assert_eq!(std::fs::read(&dst).expect("read dst"), bytes);
        assert_eq!(
            std::fs::metadata(&dst)
                .expect("dst metadata")
                .modified()
                .expect("dst mtime"),
            old_mtime
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_paths_keep_their_existing_permission_semantics() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        for through_symlink in [false, true] {
            let temp = tempfile::tempdir().expect("tempdir");
            let src = temp.path().join("src");
            let target = temp.path().join("target");
            let dst = temp.path().join("dst");
            if through_symlink {
                std::os::unix::fs::symlink(&target, &dst).unwrap();
            }
            std::fs::write(&src, b"contents").expect("source");
            std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o600))
                .expect("source permissions");
            std::fs::write(&dst, b"previous").expect("destination");
            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o640))
                .expect("destination permissions");

            platform_copy_blocking(&src, &dst).expect("platform copy");
            assert_eq!(std::fs::metadata(&dst).unwrap().mode() & 0o777, 0o600);

            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o640))
                .expect("reset destination permissions");
            stream_file_copy_blocking(&src, &dst).expect("stream copy");
            assert_eq!(std::fs::metadata(&dst).unwrap().mode() & 0o777, 0o640);
        }
    }

    #[cfg(unix)]
    #[test]
    fn streaming_permissions_follow_open_result_not_preflight() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        for existed_at_preflight in [false, true] {
            for through_symlink in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let src = temp.path().join("src");
                let dst = temp.path().join("dst");
                let target = temp.path().join("target");
                std::fs::write(&src, b"contents").unwrap();
                std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o700)).unwrap();
                std::fs::write(&target, b"previous").unwrap();
                let default_mode = std::fs::metadata(&target).unwrap().mode() & 0o7777;
                if !existed_at_preflight {
                    std::fs::remove_file(&target).unwrap();
                }
                if through_symlink {
                    std::os::unix::fs::symlink("target", &dst).unwrap();
                }
                let path = if through_symlink { &dst } else { &target };
                assert_eq!(
                    ensure_distinct_files(&src, path).unwrap(),
                    existed_at_preflight
                );

                let expected = if existed_at_preflight {
                    std::fs::remove_file(&target).unwrap();
                    default_mode
                } else {
                    std::fs::write(&target, b"concurrent destination").unwrap();
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    0o600
                };
                copy_open_handles_after_preflight(&src, path, false, std::io::copy).unwrap();
                assert_eq!(std::fs::read(path).unwrap(), b"contents");
                assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o7777, expected);
                if through_symlink {
                    assert!(std::fs::symlink_metadata(&dst).unwrap().is_symlink());
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn new_streaming_destination_respects_umask_077() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        // Only the subprocess shell changes umask; the shared test process and
        // its other test threads never change their process-global umask.
        const CHILD: &str = "IRIS_COPY_UMASK_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new("sh")
                .args(["-c", "umask 077; exec \"$1\" --exact copy::tests::new_streaming_destination_respects_umask_077 --nocapture", "sh"])
                .arg(std::env::current_exe().unwrap())
                .env(CHILD, "1")
                .output()
                .expect("run isolated umask test");
            assert!(
                output.status.success(),
                "child failed: {:?}\n{}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("src");
        std::fs::write(&src, b"contents").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644)).unwrap();
        for through_symlink in [false, true] {
            let dst = temp
                .path()
                .join(if through_symlink { "link" } else { "direct" });
            if through_symlink {
                std::os::unix::fs::symlink("target", &dst).unwrap();
            }
            stream_file_copy_blocking(&src, &dst).unwrap();
            assert_eq!(std::fs::read(&dst).unwrap(), b"contents");
            assert_eq!(std::fs::metadata(&dst).unwrap().mode() & 0o7777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn new_copy_targets_are_restrictive_before_writing_and_after_partial_failure() {
        use std::io::Write;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        for (copy_permissions, through_symlink) in
            [(true, false), (true, true), (false, false), (false, true)]
        {
            let temp = tempfile::tempdir().expect("tempdir");
            let src = temp.path().join("src");
            let dst = temp.path().join("dst");
            std::fs::write(&src, b"contents").unwrap();
            std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644)).unwrap();

            if through_symlink {
                std::os::unix::fs::symlink("target", &dst).unwrap();
            }
            let error =
                copy_open_handles_after_preflight(&src, &dst, copy_permissions, |_, target| {
                    let metadata = target.metadata()?;
                    assert_eq!(metadata.len(), 0);
                    assert_eq!(metadata.mode() & 0o7777 & !0o600, 0);
                    target.write_all(b"partial")?;
                    Err(io::Error::other("simulated copy failure"))
                })
                .expect_err("copy must fail after writing partial data");

            assert!(matches!(error, PlatformCopyError::Io(ref e)
                if e.to_string() == "simulated copy failure"));
            assert_eq!(std::fs::read(&dst).unwrap(), b"partial");
            assert_eq!(std::fs::metadata(&dst).unwrap().mode() & 0o7777 & !0o600, 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn new_copy_targets_receive_final_permissions_only_after_success() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        for (copy, copy_permissions) in [
            (platform_copy_blocking as fn(&Path, &Path) -> _, true),
            (stream_file_copy_blocking, false),
        ] {
            // Exercise direct and dangling-symlink destinations while preserving
            // the existing link-following behavior.
            for link_kind in ["direct", "relative", "absolute"] {
                let temp = tempfile::tempdir().expect("tempdir");
                let src = temp.path().join("src");
                let dst = temp.path().join("dst");
                let target = temp.path().join("target");
                std::fs::write(&src, b"contents").unwrap();
                std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644)).unwrap();
                // A normal creation in this directory measures the current
                // umask/default ACL without changing process-global state.
                std::fs::write(&target, b"").unwrap();
                let default_mode = std::fs::metadata(&target).unwrap().mode() & 0o7777;
                std::fs::remove_file(&target).unwrap();
                match link_kind {
                    "relative" => std::os::unix::fs::symlink("target", &dst).unwrap(),
                    "absolute" => std::os::unix::fs::symlink(&target, &dst).unwrap(),
                    _ => {}
                }

                assert_eq!(copy(&src, &dst).expect("copy"), 8);
                assert_eq!(std::fs::read(&dst).unwrap(), b"contents");
                let mode = std::fs::metadata(&dst).unwrap().mode() & 0o7777;
                let expected = if copy_permissions {
                    0o644
                } else {
                    default_mode
                };
                assert_eq!(mode, expected);
                if link_kind != "direct" {
                    assert!(std::fs::symlink_metadata(&dst).unwrap().is_symlink());
                    assert_eq!(std::fs::read(&target).unwrap(), b"contents");
                }
            }
        }
    }

    #[tokio::test]
    async fn stream_file_copy_rejects_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dst = temp.path().join("dst");
        let error = stream_file_copy(temp.path(), &dst)
            .await
            .expect_err("directory should not be copied by stream_file_copy");
        assert!(error.should_fallback_to_streaming());
    }

    #[tokio::test]
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    async fn large_unprogressed_platform_copy_uses_streaming_fallback() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("large.bin");
        let dst = temp.path().join("dst.bin");
        let file = std::fs::File::create(&src).expect("create src");
        file.set_len(PLATFORM_COPY_SIZE_LIMIT + 1)
            .expect("make sparse src");

        let error = platform_copy(&src, &dst)
            .await
            .expect_err("large copy should use streaming fallback");

        assert!(error.should_fallback_to_streaming());
        assert!(!dst.exists());
    }
}
