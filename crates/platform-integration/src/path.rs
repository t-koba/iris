use crate::error::PlatformError;
use std::path::{Component, Path, PathBuf};

/// Resolves `child` under `parent`, rejecting paths that escape it (directory traversal).
pub fn ensure_within(parent: &Path, child: &Path) -> Result<PathBuf, PlatformError> {
    let resolved = resolve_within(parent, child)?;
    if resolved.starts_with(parent) {
        Ok(resolved)
    } else {
        Err(PlatformError::Path {
            message: format!("path {} escapes root {}", child.display(), parent.display()),
        })
    }
}

/// Logical resolution without canonicalize, so it also works for paths that do not exist yet.
fn resolve_within(parent: &Path, child: &Path) -> Result<PathBuf, PlatformError> {
    let mut result = parent.to_path_buf();
    for component in child.components() {
        match component {
            Component::Normal(c) => result.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.starts_with(parent) || result == parent {
                    return Err(PlatformError::Path {
                        message: format!(
                            "path {} escapes root {}",
                            child.display(),
                            parent.display()
                        ),
                    });
                }
                result.pop();
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(PlatformError::Path {
                    message: format!("absolute path not allowed: {}", child.display()),
                });
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_within_normal() {
        let parent = Path::new("/home/user/data");
        let child = Path::new("subdir/file.txt");
        let result = resolve_within(parent, child).unwrap();
        assert_eq!(result, PathBuf::from("/home/user/data/subdir/file.txt"));
    }

    #[test]
    fn test_resolve_within_traversal_rejected() {
        let parent = Path::new("/home/user/data");
        let child = Path::new("../../etc/passwd");
        assert!(resolve_within(parent, child).is_err());
    }

    #[test]
    fn test_resolve_within_curdir() {
        let parent = Path::new("/home/user/data");
        let child = Path::new("./subdir/./file.txt");
        let result = resolve_within(parent, child).unwrap();
        assert_eq!(result, PathBuf::from("/home/user/data/subdir/file.txt"));
    }

    #[test]
    fn test_resolve_within_absolute_rejected() {
        let parent = Path::new("/home/user/data");
        let child = Path::new("/etc/passwd");
        assert!(resolve_within(parent, child).is_err());
    }
}
