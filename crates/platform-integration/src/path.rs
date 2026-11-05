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

/// `photo.jpg` -> the first free `photo (1).jpg`, `photo (2).jpg`, ... next to it.
///
/// A dangling symlink counts as taken: creating a file at its path would fail.
pub fn unique_sibling_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    (1u64..)
        .map(|index| path.with_file_name(format!("{stem} ({index}){ext}")))
        .find(|candidate| std::fs::symlink_metadata(candidate).is_err())
        .unwrap_or_else(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_sibling_path_skips_taken_names_including_dangling_links() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("photo.jpg");
        std::fs::write(&original, b"x").expect("write original");
        std::fs::write(temp.path().join("photo (1).jpg"), b"x").expect("write first copy");
        #[cfg(unix)]
        std::os::unix::fs::symlink("missing", temp.path().join("photo (2).jpg"))
            .expect("dangling link");

        let expected = if cfg!(unix) {
            "photo (3).jpg"
        } else {
            "photo (2).jpg"
        };
        assert_eq!(unique_sibling_path(&original), temp.path().join(expected));
        assert_eq!(
            unique_sibling_path(&temp.path().join("README")),
            temp.path().join("README (1)")
        );
    }

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
