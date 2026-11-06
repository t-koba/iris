use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(0);

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

    try_reflink_copy_at(src, dst, staging_path(dst))
}

fn try_reflink_copy_at(src: &Path, dst: &Path, staging: PathBuf) -> Result<bool, io::Error> {
    let staging = match clone_new_file(src, &staging) {
        Ok(Some(staging)) => staging,
        Ok(None) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) if is_unsupported_clone_error(&error) => return Ok(false),
        Err(error) => return Err(error),
    };

    install_staged_clone(staging, dst)?;
    Ok(true)
}

struct OwnedStagingFile {
    path: PathBuf,
    remove_on_drop: bool,
}

impl OwnedStagingFile {
    /// Call only after this operation has created the staging file exclusively.
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            remove_on_drop: true,
        }
    }

    fn retain(&mut self) {
        self.remove_on_drop = false;
    }
}

impl Drop for OwnedStagingFile {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn install_staged_clone(mut staging: OwnedStagingFile, dst: &Path) -> Result<(), io::Error> {
    std::fs::rename(&staging.path, dst)?;
    staging.retain();
    Ok(())
}

fn staging_path(dst: &Path) -> PathBuf {
    let id = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
    dst.with_file_name(format!(".iris-clone-{}-{id}", std::process::id()))
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
fn clone_new_file(src: &Path, dst: &Path) -> Result<Option<OwnedStagingFile>, io::Error> {
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
        Ok(Some(OwnedStagingFile::new(dst)))
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `FICLONE` shares the data blocks but the destination is a new inode: it starts with the
/// current time and default permissions, so exactly those two are set from the source.
#[cfg(target_os = "linux")]
fn clone_new_file(src: &Path, dst: &Path) -> Result<Option<OwnedStagingFile>, io::Error> {
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

    let staging = OwnedStagingFile::new(dst);
    let result = unsafe { ioctl(dst_file.as_raw_fd(), FICLONE, src_file.as_raw_fd()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }

    dst_file.set_permissions(src_metadata.permissions())?;
    if let Ok(mtime) = src_metadata.modified() {
        iris_platform_integration::file_times::preserve_modified_time_of(&dst_file, mtime)?;
    }
    Ok(Some(staging))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clone_new_file(_src: &Path, _dst: &Path) -> Result<Option<OwnedStagingFile>, io::Error> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn occupied_staging_candidate_is_never_removed_or_overwritten() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("src.bin");
        let dst = temp.path().join("dst.bin");
        let staging = temp.path().join(".iris-clone-existing");
        std::fs::write(&src, b"source contents").expect("write source");
        std::fs::write(&staging, b"unrelated contents").expect("write unrelated file");

        let result = try_reflink_copy_at(&src, &dst, staging.clone());

        assert!(matches!(result, Ok(false)));
        assert_eq!(
            std::fs::read(&staging).expect("read unrelated file"),
            b"unrelated contents"
        );
        assert!(!dst.exists());
    }

    #[test]
    fn overlapping_installs_use_and_clean_only_their_own_staging_files() {
        use std::sync::{Arc, Barrier};

        let temp = tempfile::tempdir().expect("tempdir");
        let dst = temp.path().join("dst.bin");
        let first_path = staging_path(&dst);
        let second_path = staging_path(&dst);
        assert_ne!(first_path, second_path);
        std::fs::write(&first_path, b"first clone").expect("write first clone");
        std::fs::write(&second_path, b"second clone").expect("write second clone");
        let barrier = Arc::new(Barrier::new(2));
        let first_barrier = Arc::clone(&barrier);
        let first_dst = dst.clone();
        let first_thread_path = first_path.clone();
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            install_staged_clone(OwnedStagingFile::new(&first_thread_path), &first_dst)
        });
        let second_barrier = Arc::clone(&barrier);
        let second_dst = dst.clone();
        let second_thread_path = second_path.clone();
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            install_staged_clone(OwnedStagingFile::new(&second_thread_path), &second_dst)
        });

        let _ = first.join().expect("first install thread");
        let _ = second.join().expect("second install thread");
        let installed = std::fs::read(&dst).expect("one complete clone must be installed");
        assert!(installed == b"first clone" || installed == b"second clone");
        assert!(!first_path.exists());
        assert!(!second_path.exists());
    }

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
