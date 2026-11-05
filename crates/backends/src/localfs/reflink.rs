use std::path::Path;

/// Tries a reflink (copy-on-write) copy on APFS, Btrfs or XFS.
/// Returns `Ok(false)` when the file system does not support reflink.
pub fn try_reflink_copy(src: &Path, dst: &Path) -> Result<bool, std::io::Error> {
    #[cfg(target_os = "macos")]
    {
        try_clonefile(src, dst)
    }

    #[cfg(target_os = "linux")]
    {
        return try_ficlone(src, dst);
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (src, dst);
        Ok(false)
    }
}

#[cfg(target_os = "macos")]
fn try_clonefile(src: &Path, dst: &Path) -> Result<bool, std::io::Error> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    extern "C" {
        fn clonefile(
            src: *const std::os::raw::c_char,
            dst: *const std::os::raw::c_char,
            flags: u32,
        ) -> std::os::raw::c_int;
    }

    let src_cstr = CString::new(src.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
    let dst_cstr = CString::new(dst.as_os_str().as_bytes()).map_err(std::io::Error::other)?;

    const ENOTSUP: i32 = 45; // macOS ENOTSUP

    let result = unsafe { clonefile(src_cstr.as_ptr(), dst_cstr.as_ptr(), 0) };
    if result == 0 {
        Ok(true)
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(ENOTSUP) {
            Ok(false)
        } else {
            Err(err)
        }
    }
}

#[cfg(target_os = "linux")]
fn try_ficlone(src: &Path, dst: &Path) -> Result<bool, std::io::Error> {
    use std::fs::File;
    use std::os::unix::io::AsRawFd;

    const FICLONE: u64 = 0x40049409;
    const EOPNOTSUPP: i32 = 95; // Linux EOPNOTSUPP
    const ENOTSUP: i32 = 95; // Linux ENOTSUP (same as EOPNOTSUPP)

    extern "C" {
        fn ioctl(fd: std::os::raw::c_int, request: u64, ...) -> std::os::raw::c_int;
    }

    let src_file = File::open(src)?;
    let dst_file = File::create(dst)?;

    let result = unsafe { ioctl(dst_file.as_raw_fd(), FICLONE, src_file.as_raw_fd()) };
    if result == 0 {
        Ok(true)
    } else {
        let err = std::io::Error::last_os_error();
        // EOPNOTSUPP or ENOTSUP means filesystem doesn't support reflink
        if err.raw_os_error() == Some(EOPNOTSUPP) || err.raw_os_error() == Some(ENOTSUP) {
            let _ = std::fs::remove_file(dst);
            Ok(false)
        } else {
            let _ = std::fs::remove_file(dst);
            Err(err)
        }
    }
}

pub fn detect_reflink_support(dir: &Path) -> Result<bool, std::io::Error> {
    let test_src = dir.join(".iris_reflink_test_src");
    let test_dst = dir.join(".iris_reflink_test_dst");

    let result = (|| -> Result<bool, std::io::Error> {
        std::fs::write(&test_src, b"reflink test")?;
        let supported = try_reflink_copy(&test_src, &test_dst)?;
        Ok(supported)
    })();

    let _ = std::fs::remove_file(&test_src);
    let _ = std::fs::remove_file(&test_dst);

    match result {
        Ok(supported) => Ok(supported),
        Err(err) if should_fallback_to_optimistic_probe(&err) => Ok(platform_may_support_reflink()),
        Err(err) => Err(err),
    }
}

fn should_fallback_to_optimistic_probe(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
    )
}

fn platform_may_support_reflink() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}
