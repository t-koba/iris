//! Modification times of files and directories that a copy had to create.
//!
//! Copies never touch metadata that the operation leaves alone (renames, clones that keep the
//! source's metadata). Creating a file or directory, however, always stamps the current time, so
//! that one value is set back to the source's, and only when it actually differs.

use std::fs::{File, FileTimes};
use std::io;
use std::path::Path;
use std::time::SystemTime;

/// Gives `path` the modification time `mtime` unless it already has it.
///
/// Returns whether the time had to be changed.
pub fn preserve_modified_time(path: &Path, mtime: SystemTime) -> io::Result<bool> {
    let metadata = std::fs::metadata(path)?;
    if metadata.modified().ok() == Some(mtime) {
        return Ok(false);
    }
    open_for_times(path, metadata.is_dir())?.set_times(FileTimes::new().set_modified(mtime))?;
    Ok(true)
}

/// Gives the open file `file` the modification time `mtime` unless it already has it.
///
/// Used by writers right before they close the file, so no separate open is needed.
pub fn preserve_modified_time_of(file: &File, mtime: SystemTime) -> io::Result<bool> {
    if file.metadata()?.modified().ok() == Some(mtime) {
        return Ok(false);
    }
    file.set_modified(mtime)?;
    Ok(true)
}

/// `futimens` only needs ownership, not write access, so a read-only descriptor works for files
/// (including read-only ones) and directories alike.
#[cfg(unix)]
fn open_for_times(path: &Path, _is_dir: bool) -> io::Result<File> {
    File::open(path)
}

/// `SetFileTime` needs `FILE_WRITE_ATTRIBUTES`; directories also need backup semantics to open.
#[cfg(windows)]
fn open_for_times(path: &Path, is_dir: bool) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    let mut options = std::fs::OpenOptions::new();
    options.access_mode(FILE_WRITE_ATTRIBUTES);
    if is_dir {
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    options.open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_for_times(path: &Path, _is_dir: bool) -> io::Result<File> {
    std::fs::OpenOptions::new().write(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn mtime(path: &Path) -> SystemTime {
        std::fs::metadata(path)
            .expect("metadata")
            .modified()
            .expect("mtime")
    }

    #[test]
    fn preserve_sets_file_time_only_when_it_differs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("file.bin");
        std::fs::write(&path, b"data").expect("write");
        let wanted = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);

        assert!(preserve_modified_time(&path, wanted).expect("first preserve"));
        assert_eq!(mtime(&path), wanted);
        assert!(!preserve_modified_time(&path, wanted).expect("second preserve"));
    }

    #[test]
    fn preserve_sets_directory_time() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("dir");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::write(dir.join("child"), b"x").expect("write child");
        let wanted = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);

        assert!(preserve_modified_time(&dir, wanted).expect("preserve dir"));
        assert_eq!(mtime(&dir), wanted);
    }

    #[cfg(unix)]
    #[test]
    fn preserve_works_on_read_only_files() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("read-only.bin");
        std::fs::write(&path, b"data").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).expect("chmod");
        let wanted = SystemTime::UNIX_EPOCH + Duration::from_secs(1_400_000_000);

        preserve_modified_time(&path, wanted).expect("preserve read-only");
        assert_eq!(mtime(&path), wanted);
    }

    #[test]
    fn preserve_open_file_skips_matching_time() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("open.bin");
        let file = File::create(&path).expect("create");
        let wanted = SystemTime::UNIX_EPOCH + Duration::from_secs(1_300_000_000);

        assert!(preserve_modified_time_of(&file, wanted).expect("first"));
        assert!(!preserve_modified_time_of(&file, wanted).expect("second"));
        drop(file);
        assert_eq!(mtime(&path), wanted);
    }
}
