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
    metadata: &Metadata,
) -> Result<StableId, BackendError> {
    use std::os::windows::fs::MetadataExt;

    let volume_serial = metadata.volume_serial_number().ok_or_else(|| {
        BackendError::Internal(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "volume serial number is unavailable for path '{}'",
                path.display()
            ),
        )))
    })?;
    let file_index = metadata.file_index().ok_or_else(|| {
        BackendError::Internal(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("file index is unavailable for path '{}'", path.display()),
        )))
    })?;
    Ok(StableId(format!("{}:{}", volume_serial, file_index)))
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
