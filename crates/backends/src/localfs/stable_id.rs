use iris_core_domain::types::error::BackendError;
use iris_core_domain::types::object::StableId;
use std::fs::Metadata;
use std::path::Path;

/// Unix: inode + device id. Windows: volume serial number + file index.
#[cfg(unix)]
pub fn get_stable_id(path: &Path) -> Result<StableId, BackendError> {
    let metadata = std::fs::metadata(path).map_err(|e| BackendError::Internal(Box::new(e)))?;
    get_stable_id_from_metadata(path, &metadata)
}

#[cfg(unix)]
pub fn get_stable_id_from_metadata(
    _path: &Path,
    metadata: &Metadata,
) -> Result<StableId, BackendError> {
    use std::os::unix::fs::MetadataExt;

    let dev = metadata.dev();
    let ino = metadata.ino();
    Ok(StableId(format!("{}:{}", dev, ino)))
}

#[cfg(windows)]
pub fn get_stable_id(path: &Path) -> Result<StableId, BackendError> {
    let metadata = std::fs::metadata(path).map_err(|e| BackendError::Internal(Box::new(e)))?;
    get_stable_id_from_metadata(path, &metadata)
}

#[cfg(windows)]
pub fn get_stable_id_from_metadata(
    path: &Path,
    _metadata: &Metadata,
) -> Result<StableId, BackendError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
    };

    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|error| BackendError::Internal(Box::new(error)))?;
    let handle = winapi_util::Handle::from_file(file);
    let information = winapi_util::file::information(&handle)
        .map_err(|error| BackendError::Internal(Box::new(error)))?;
    Ok(StableId(format!(
        "{}:{}",
        information.volume_serial_number(),
        information.file_index()
    )))
}

#[cfg(not(any(unix, windows)))]
pub fn get_stable_id(path: &Path) -> Result<StableId, BackendError> {
    let metadata = std::fs::metadata(path).map_err(|e| BackendError::Internal(Box::new(e)))?;
    get_stable_id_from_metadata(path, &metadata)
}

#[cfg(not(any(unix, windows)))]
pub fn get_stable_id_from_metadata(
    path: &Path,
    _metadata: &Metadata,
) -> Result<StableId, BackendError> {
    // Fallback: hash of the path.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let canonical = path
        .canonicalize()
        .map_err(|e| BackendError::Internal(Box::new(e)))?;

    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    Ok(StableId(format!("path:{}", hasher.finish())))
}
