use crate::error::PlatformError;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrashReceipt {
    pub original_path: PathBuf,
    pub trashed_path: Option<PathBuf>,
}

/// Moves a file to the trash / Recycle Bin.
pub fn trash_file(path: &Path) -> Result<(), PlatformError> {
    trash_file_with_receipt(path).map(|_| ())
}

pub fn trash_file_with_receipt(path: &Path) -> Result<TrashReceipt, PlatformError> {
    trash_file_with_receipt_impl(path)
}

#[cfg(not(target_os = "macos"))]
fn trash_file_with_receipt_impl(path: &Path) -> Result<TrashReceipt, PlatformError> {
    trash::delete(path).map_err(|e| PlatformError::Trash {
        message: format!("{}: {}", path.display(), e),
    })?;
    Ok(TrashReceipt {
        original_path: path.to_path_buf(),
        trashed_path: None,
    })
}

#[cfg(target_os = "macos")]
fn trash_file_with_receipt_impl(path: &Path) -> Result<TrashReceipt, PlatformError> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    let path_str = path.to_string_lossy();
    let ns_path = NSString::from_str(&path_str);
    let url = NSURL::fileURLWithPath(&ns_path);
    let manager = NSFileManager::defaultManager();
    let mut resulting_url = None;
    manager
        .trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting_url))
        .map_err(|error| PlatformError::Trash {
            message: format!("{}: {}", path.display(), error),
        })?;
    let trashed_path = resulting_url
        .and_then(|url| url.path())
        .map(|path| PathBuf::from(path.to_string()));
    Ok(TrashReceipt {
        original_path: path.to_path_buf(),
        trashed_path,
    })
}

#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
pub fn restore_files_with_receipts(receipts: &[TrashReceipt]) -> Result<(), PlatformError> {
    let mut items = trash::os_limited::list().map_err(|e| PlatformError::Trash {
        message: e.to_string(),
    })?;
    let mut restore_items = Vec::with_capacity(receipts.len());
    for receipt in receipts {
        let Some(position) = items
            .iter()
            .position(|item| item.original_path() == receipt.original_path)
        else {
            return Err(PlatformError::Trash {
                message: format!(
                    "{} is not available in Trash",
                    receipt.original_path.display()
                ),
            });
        };
        restore_items.push(items.remove(position));
    }

    trash::os_limited::restore_all(restore_items).map_err(|e| PlatformError::Trash {
        message: e.to_string(),
    })
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    all(unix, not(target_os = "ios"), not(target_os = "android"))
)))]
pub fn restore_files_with_receipts(_receipts: &[TrashReceipt]) -> Result<(), PlatformError> {
    Err(PlatformError::Trash {
        message: "Trash restore is not supported on this platform".to_string(),
    })
}

#[cfg(target_os = "macos")]
pub fn restore_files_with_receipts(receipts: &[TrashReceipt]) -> Result<(), PlatformError> {
    for receipt in receipts {
        let Some(trashed_path) = &receipt.trashed_path else {
            return Err(PlatformError::Trash {
                message: format!(
                    "{} is missing a Trash receipt",
                    receipt.original_path.display()
                ),
            });
        };
        if receipt.original_path.exists() {
            return Err(PlatformError::Trash {
                message: format!("{} already exists", receipt.original_path.display()),
            });
        }
        if let Some(parent) = receipt.original_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| PlatformError::Trash {
                message: format!("{}: {}", parent.display(), error),
            })?;
        }
        std::fs::rename(trashed_path, &receipt.original_path).map_err(|error| {
            PlatformError::Trash {
                message: format!(
                    "{} -> {}: {}",
                    trashed_path.display(),
                    receipt.original_path.display(),
                    error
                ),
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    #[ignore] // Requires Finder process; may timeout in CI
    fn test_trash_file() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test_trash.txt");
        fs::write(&file_path, "trash me").unwrap();
        assert!(file_path.exists());

        trash_file(&file_path).unwrap();
        assert!(!file_path.exists());
    }
}
