use std::io;
use std::path::{Path, PathBuf};

/// Whether this platform has a copy-on-write clone call worth trying (APFS, Btrfs, XFS).
///
/// Support depends on the file system of each copy, so it is not probed up front (probing would
/// have to create and delete files in the user's directories); [`try_reflink_copy`] falls back
/// per copy instead.
pub fn platform_may_support_reflink() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// Tries a copy-on-write clone of `src` to `dst`, replacing `dst` if it exists.
///
/// Returns `Ok(false)`, leaving `dst` untouched, when the file system or the pair of paths cannot
/// be cloned; the caller then copies the bytes. The clone is made under a temporary name and
/// renamed into place, so an existing destination is replaced only by a complete clone.
pub fn try_reflink_copy(src: &Path, dst: &Path) -> Result<bool, io::Error> {
    if !platform_may_support_reflink() {
        return Ok(false);
    }

    let staging = staging_path(dst);
    let _ = std::fs::remove_file(&staging);
    let result = clone_new_file(src, &staging).and_then(|cloned| {
        if cloned {
            std::fs::rename(&staging, dst)?;
        }
        Ok(cloned)
    });
    if !matches!(result, Ok(true)) {
        let _ = std::fs::remove_file(&staging);
    }
    match result {
        Err(error) if is_unsupported_clone_error(&error) => Ok(false),
        other => other,
    }
}

fn staging_path(dst: &Path) -> PathBuf {
    let name = dst
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    dst.with_file_name(format!(".{name}.iris-clone-{}", std::process::id()))
}

/// Errors that only mean "this copy cannot be cloned", as opposed to I/O failures.
fn is_unsupported_clone_error(error: &io::Error) -> bool {
    #[cfg(target_os = "linux")]
    const UNSUPPORTED: &[i32] = &[
        1,  // EPERM: immutable/append-only or a file system refusing clones
        18, // EXDEV: different file systems
        22, // EINVAL: unaligned or unsupported on this file system
        25, // ENOTTY: the ioctl does not exist here (FUSE, 9p, ...)
        30, // EROFS
        38, // ENOSYS
        95, // EOPNOTSUPP / ENOTSUP
    ];
    #[cfg(target_os = "macos")]
    const UNSUPPORTED: &[i32] = &[
        1,   // EPERM
        18,  // EXDEV
        30,  // EROFS
        45,  // ENOTSUP
        102, // EOPNOTSUPP
    ];
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    const UNSUPPORTED: &[i32] = &[];

    error
        .raw_os_error()
        .is_some_and(|code| UNSUPPORTED.contains(&code))
}

/// `clonefile` copies data and metadata (mode, times, xattrs) like `cp -c`.
#[cfg(target_os = "macos")]
fn clone_new_file(src: &Path, dst: &Path) -> Result<bool, io::Error> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    extern "C" {
        fn clonefile(
            src: *const std::os::raw::c_char,
            dst: *const std::os::raw::c_char,
            flags: u32,
        ) -> std::os::raw::c_int;
    }

    let src_cstr = CString::new(src.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let dst_cstr = CString::new(dst.as_os_str().as_bytes()).map_err(io::Error::other)?;

    let result = unsafe { clonefile(src_cstr.as_ptr(), dst_cstr.as_ptr(), 0) };
    if result == 0 {
        Ok(true)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `FICLONE` shares the data blocks but the destination is a new inode: it starts with the
/// current time and default permissions, so exactly those two are set from the source.
#[cfg(target_os = "linux")]
fn clone_new_file(src: &Path, dst: &Path) -> Result<bool, io::Error> {
    use std::fs::File;
    use std::os::unix::io::AsRawFd;

    const FICLONE: u64 = 0x40049409;

    extern "C" {
        fn ioctl(fd: std::os::raw::c_int, request: u64, ...) -> std::os::raw::c_int;
    }

    let src_file = File::open(src)?;
    let src_metadata = src_file.metadata()?;
    let dst_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)?;

    let result = unsafe { ioctl(dst_file.as_raw_fd(), FICLONE, src_file.as_raw_fd()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }

    dst_file.set_permissions(src_metadata.permissions())?;
    if let Ok(mtime) = src_metadata.modified() {
        iris_platform_integration::file_times::preserve_modified_time_of(&dst_file, mtime)?;
    }
    Ok(true)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clone_new_file(_src: &Path, _dst: &Path) -> Result<bool, io::Error> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn reflink_copy_replaces_existing_destination_or_leaves_it_untouched() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("src.bin");
        let dst = temp.path().join("dst.bin");
        std::fs::write(&src, b"new contents").expect("write src");
        std::fs::write(&dst, b"old").expect("write dst");
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_234_567_890);
        iris_platform_integration::file_times::preserve_modified_time(&src, mtime)
            .expect("set src mtime");

        let cloned = try_reflink_copy(&src, &dst).expect("clone attempt");

        let contents = std::fs::read(&dst).expect("read dst");
        if cloned {
            assert_eq!(contents, b"new contents");
            assert_eq!(
                std::fs::metadata(&dst)
                    .expect("dst metadata")
                    .modified()
                    .expect("dst mtime"),
                mtime
            );
        } else {
            assert_eq!(contents, b"old");
        }
        let leftovers = std::fs::read_dir(temp.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("iris-clone"))
            .count();
        assert_eq!(leftovers, 0, "staging file must not be left behind");
    }
}
