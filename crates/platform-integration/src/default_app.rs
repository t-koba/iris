use crate::error::PlatformError;
use std::path::Path;

/// Opens a file with the OS default application.
pub fn open_with_default_app(path: &Path) -> Result<(), PlatformError> {
    open::that(path).map_err(|e| PlatformError::DefaultApp {
        message: format!("{}: {}", path.display(), e),
    })
}

pub fn open_url(url: &str) -> Result<(), PlatformError> {
    open::that(url).map_err(|e| PlatformError::DefaultApp {
        message: format!("open {}: {}", url, e),
    })
}
