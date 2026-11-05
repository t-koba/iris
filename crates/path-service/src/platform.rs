#[cfg(windows)]
use crate::normalize::normalize_local_path;
use std::path::Path;
#[cfg(windows)]
use std::path::{Component, PathBuf};

#[cfg(windows)]
fn drive_root(path: &Path) -> Option<PathBuf> {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return None;
    };

    let disk = match prefix.kind() {
        std::path::Prefix::Disk(letter) | std::path::Prefix::VerbatimDisk(letter) => {
            format!("{}:\\", char::from(letter))
        }
        _ => return None,
    };

    Some(PathBuf::from(disk))
}

#[cfg(windows)]
pub fn is_boot_volume_path(path: &Path) -> bool {
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    let system_root = if system_drive.ends_with('\\') {
        PathBuf::from(system_drive)
    } else {
        PathBuf::from(format!("{}\\", system_drive))
    };

    match (drive_root(path), drive_root(&system_root)) {
        (Some(path_root), Some(system_root)) => normalize_local_path(&path_root)
            .eq_ignore_ascii_case(&normalize_local_path(&system_root)),
        _ => false,
    }
}

#[cfg(not(windows))]
pub fn is_boot_volume_path(path: &Path) -> bool {
    same_filesystem(path, Path::new("/"))
}

#[cfg(unix)]
pub fn same_filesystem(src: &Path, dst: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let src_dev = std::fs::metadata(src).ok().map(|metadata| metadata.dev());
    let dst_anchor = if dst.exists() {
        dst
    } else {
        dst.parent().unwrap_or(dst)
    };
    let dst_dev = std::fs::metadata(dst_anchor)
        .ok()
        .map(|metadata| metadata.dev());
    src_dev == dst_dev
}

#[cfg(not(unix))]
pub fn same_filesystem(src: &Path, dst: &Path) -> bool {
    src.components().next() == dst.components().next()
}
