use crate::error::PlatformError;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrashReceipt {
    pub original_path: PathBuf,
    pub trashed_path: Option<PathBuf>,
    /// Stable system Trash item ID on Windows/Freedesktop, not a restore pathname.
    pub trash_id: Option<OsString>,
    /// Backend original path captured before deletion; never recomputed during restore.
    pub normalized_path: Option<PathBuf>,
}

/// Moves a file to the trash / Recycle Bin.
#[cfg(not(target_os = "macos"))]
pub fn trash_file(path: &Path) -> Result<(), PlatformError> {
    trash::delete(path).map_err(|e| PlatformError::Trash {
        message: format!("{}: {}", path.display(), e),
    })
}

/// Moves a file to the trash using NSFileManager.
#[cfg(target_os = "macos")]
pub fn trash_file(path: &Path) -> Result<(), PlatformError> {
    trash_file_with_receipt(path).map(|_| ())
}

/// Moves an item to Trash and returns the identity needed for undo.
pub fn trash_file_with_receipt(path: &Path) -> Result<TrashReceipt, PlatformError> {
    trash_files_with_receipts(std::slice::from_ref(&path.to_path_buf()))
        .map(|mut receipts| receipts.remove(0))
}

/// Moves a selection to Trash and returns receipts in selection order.
///
/// On Windows/Freedesktop, one snapshot before and one after the entire batch
/// must identify exactly one new system ID per original path. Missing or ambiguous
/// candidates return an error even though deletion has already occurred; no receipt
/// is guessed. This cannot prove attribution under arbitrary external Trash
/// manipulation, such as removal and ID reuse. Paths are parent-normalized and
/// duplicates rejected before deletion; receipts retain the caller path separately.
/// macOS uses each resulting URL.
pub fn trash_files_with_receipts(paths: &[PathBuf]) -> Result<Vec<TrashReceipt>, PlatformError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    trash_files_with_receipts_impl(paths)
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
fn trash_files_with_receipts_impl(paths: &[PathBuf]) -> Result<Vec<TrashReceipt>, PlatformError> {
    let normalized_paths = exact_items::normalize_paths(paths)?;
    // One snapshot on each side of the batch: no retries or ordering heuristics.
    let before = trash::os_limited::list().map_err(|e| PlatformError::Trash {
        message: format!("Cannot enumerate Trash before deleting selection: {e}"),
    })?;
    for path in &normalized_paths {
        trash_file(path)?;
    }
    let after = trash::os_limited::list().map_err(|e| PlatformError::Trash {
        message: format!("Deleted selection, but cannot enumerate Trash for undo: {e}"),
    })?;
    exact_items::bind_receipts(paths, &normalized_paths, before, after)
}

#[cfg(not(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
)))]
fn trash_files_with_receipts_impl(paths: &[PathBuf]) -> Result<Vec<TrashReceipt>, PlatformError> {
    paths
        .iter()
        .map(|path| trash_file_with_receipt_impl(path))
        .collect()
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    all(unix, not(target_os = "ios"), not(target_os = "android"))
)))]
fn trash_file_with_receipt_impl(path: &Path) -> Result<TrashReceipt, PlatformError> {
    trash::delete(path).map_err(|e| PlatformError::Trash {
        message: format!("{}: {}", path.display(), e),
    })?;
    Ok(TrashReceipt {
        original_path: path.to_path_buf(),
        trashed_path: None,
        trash_id: None,
        normalized_path: None,
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
        trash_id: None,
        normalized_path: None,
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
    let items = trash::os_limited::list().map_err(|e| PlatformError::Trash {
        message: e.to_string(),
    })?;
    // Validate the entire selection before asking the backend to restore anything.
    let restore_items = exact_items::select_restore_items(items, receipts)?;
    trash::os_limited::restore_all(restore_items).map_err(|e| PlatformError::Trash {
        message: e.to_string(),
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
mod exact_items {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use trash::TrashItem;

    // Match trash 5.3.0's canonicalize_paths: resolve only the parent, not the
    // final component (which may itself be a symlink). Freeze all paths before
    // deletion, including rejecting aliases of the same selection entry.
    pub(super) fn normalize_paths(paths: &[PathBuf]) -> Result<Vec<PathBuf>, PlatformError> {
        let mut seen = HashSet::with_capacity(paths.len());
        paths
            .iter()
            .map(|path| {
                let error = || PlatformError::Trash {
                    message: format!("Cannot normalize a unique Trash path: {}", path.display()),
                };
                // A terminal `..` has no file name; the fallback below would
                // select its parent instead of the directory the caller named.
                if path.as_os_str().is_empty()
                    || matches!(
                        path.components().next_back(),
                        Some(std::path::Component::ParentDir)
                    )
                {
                    return Err(error());
                }
                let absolute = if path.is_relative() {
                    std::env::current_dir().map_err(|_| error())?.join(path)
                } else {
                    path.clone()
                };
                let parent = absolute.parent().ok_or_else(error)?;
                let parent = parent.canonicalize().map_err(|_| error())?;
                let normalized = match absolute.file_name() {
                    Some(name) => parent.join(name),
                    None => parent,
                };
                // The Windows backend converts canonical verbatim paths for the Shell.
                #[cfg(target_os = "windows")]
                let normalized = shell_path(normalized);
                if !seen.insert(normalized.clone()) {
                    return Err(error());
                }
                Ok(normalized)
            })
            .collect()
    }

    #[cfg(target_os = "windows")]
    fn shell_path(path: PathBuf) -> PathBuf {
        use std::path::{Component, Prefix};
        let mut components = path.components();
        let mut prefix = OsString::new();
        match components.next() {
            Some(Component::Prefix(component)) => match component.kind() {
                Prefix::VerbatimDisk(drive) => prefix.push(format!("{}:", drive as char)),
                Prefix::VerbatimUNC(host, share) => {
                    prefix.push(r"\\");
                    prefix.push(host);
                    prefix.push(r"\");
                    prefix.push(share);
                }
                _ => return path,
            },
            _ => return path,
        }
        prefix.push(components.as_path());
        prefix.into()
    }

    // IDs are .trashinfo paths on Freedesktop and Shell parsing names on Windows.
    pub(super) fn bind_receipts(
        paths: &[PathBuf],
        normalized_paths: &[PathBuf],
        before: Vec<TrashItem>,
        after: Vec<TrashItem>,
    ) -> Result<Vec<TrashReceipt>, PlatformError> {
        let binding_error = |path: &Path| PlatformError::Trash {
            message: format!(
                "Deleted {}, but cannot bind a unique new Trash ID for undo",
                path.display()
            ),
        };
        let previous_ids: HashSet<_> = before.into_iter().map(|item| item.id).collect();
        let mut candidates = HashMap::with_capacity(paths.len());
        for path in normalized_paths {
            if candidates.insert(path.clone(), None).is_some() {
                return Err(binding_error(path));
            }
        }
        let mut matched_ids = HashSet::new();
        for item in after {
            if previous_ids.contains(&item.id) {
                continue;
            }
            let path = item.original_path();
            if let Some(candidate) = candidates.get_mut(&path) {
                if candidate.is_some() || !matched_ids.insert(item.id.clone()) {
                    return Err(binding_error(&path));
                }
                *candidate = Some(item.id);
            }
        }
        paths
            .iter()
            .zip(normalized_paths)
            .map(|(path, normalized_path)| {
                let id = candidates
                    .remove(normalized_path)
                    .flatten()
                    .ok_or_else(|| binding_error(path))?;
                Ok(TrashReceipt {
                    original_path: path.clone(),
                    trashed_path: None,
                    trash_id: Some(id),
                    normalized_path: Some(normalized_path.clone()),
                })
            })
            .collect()
    }

    pub(super) fn select_restore_items(
        mut items: Vec<TrashItem>,
        receipts: &[TrashReceipt],
    ) -> Result<Vec<TrashItem>, PlatformError> {
        let mut selected = Vec::with_capacity(receipts.len());
        for receipt in receipts {
            let mut matches = items
                .iter()
                .enumerate()
                .filter(|(_, item)| receipt.trash_id.as_ref() == Some(&item.id));
            let position = match (matches.next(), matches.next()) {
                (Some((position, item)), None)
                    if receipt.normalized_path.as_ref() == Some(&item.original_path()) =>
                {
                    position
                }
                _ => {
                    return Err(PlatformError::Trash {
                        message: format!(
                            "{} has no unique matching Trash ID and normalized path",
                            receipt.original_path.display()
                        ),
                    })
                }
            };
            selected.push(items.remove(position));
        }
        Ok(selected)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn item(id: &str, path: &Path) -> TrashItem {
            TrashItem {
                id: id.into(),
                name: path.file_name().unwrap().into(),
                original_parent: path.parent().unwrap().into(),
                time_deleted: 0, // Same metadata: only ID can distinguish the items.
            }
        }

        fn bind_receipt(
            path: &Path,
            before: Vec<TrashItem>,
            after: Vec<TrashItem>,
        ) -> Result<TrashReceipt, PlatformError> {
            bind_receipts(&[path.to_path_buf()], &[path.to_path_buf()], before, after)
                .map(|mut receipts| receipts.remove(0))
        }

        // Exercise pre-delete normalization and post-delete snapshots without OS Trash.
        fn assert_normalized_binding(path: PathBuf, normalized: Vec<PathBuf>) {
            let old = item("old-id", &normalized[0]);
            let new = item("new-id", &normalized[0]);
            let receipts = bind_receipts(
                std::slice::from_ref(&path),
                &normalized,
                vec![old.clone()],
                vec![new.clone(), old.clone()],
            )
            .unwrap();
            assert_eq!(receipts[0].original_path, path);
            assert_eq!(receipts[0].normalized_path.as_ref(), Some(&normalized[0]));
            assert_eq!(receipts[0].trash_id.as_ref(), Some(&new.id));
            let restored = select_restore_items(vec![old, new.clone()], &receipts).unwrap();
            assert_eq!(restored[0].id, new.id);
            assert!(select_restore_items(vec![item("new-id", &path)], &receipts,).is_err());
        }

        #[test]
        fn relative_input_binds_normalized_path_without_changing_cwd() {
            let cwd = std::env::current_dir().unwrap();
            let dir = tempfile::tempdir_in(&cwd).unwrap();
            let path = dir.path().strip_prefix(&cwd).unwrap().join("file");
            std::fs::write(&path, "selected").unwrap();
            let normalized = normalize_paths(std::slice::from_ref(&path)).unwrap();
            let expected = dir.path().canonicalize().unwrap().join("file");
            #[cfg(target_os = "windows")]
            let expected = shell_path(expected);
            assert_eq!(normalized, [expected]);
            assert!(normalize_paths(&[path.clone(), normalized[0].clone()]).is_err());
            for invalid in [PathBuf::new(), dir.path().join("missing-parent/file")] {
                assert!(normalize_paths(&[path.clone(), invalid]).is_err());
                assert!(path.exists());
            }
            std::fs::rename(&path, dir.path().join("simulated-trash")).unwrap();
            assert_normalized_binding(path, normalized);
        }

        #[test]
        fn terminal_parent_rejects_entire_batch_without_mutation() {
            let dir = tempfile::tempdir().unwrap();
            let parent = dir.path().join("a");
            let child = parent.join("b");
            std::fs::create_dir_all(&child).unwrap();
            let valid = parent.join("file");
            let child_file = child.join("retained");
            std::fs::write(&valid, "selected").unwrap();
            std::fs::write(&child_file, "untouched").unwrap();

            // Call preflight directly: even a regression must never touch OS Trash.
            let result = normalize_paths(&[valid.clone(), child.join("..")]);
            assert!(matches!(result, Err(PlatformError::Trash { .. })));
            assert!(parent.is_dir());
            assert!(child.is_dir());
            assert_eq!(std::fs::read(&valid).unwrap(), b"selected");
            assert_eq!(std::fs::read(&child_file).unwrap(), b"untouched");

            // Internal parent components still resolve via parent canonicalization.
            let normalized = normalize_paths(&[child.join("..").join("file")]).unwrap();
            let expected = parent.canonicalize().unwrap().join("file");
            #[cfg(target_os = "windows")]
            let expected = shell_path(expected);
            assert_eq!(normalized, [expected]);
        }

        #[cfg(unix)] // Symlinks need no privileges here; no process-global cwd changes.
        #[test]
        fn symlinked_parent_binds_without_dereferencing_final_symlink() {
            use std::os::unix::fs::symlink;
            let dir = tempfile::tempdir().unwrap();
            let parent = dir.path().join("real");
            let alias = dir.path().join("alias");
            std::fs::create_dir(&parent).unwrap();
            symlink(&parent, &alias).unwrap();
            symlink("target", parent.join("file")).unwrap();
            let path = alias.join("file");
            let normalized = normalize_paths(std::slice::from_ref(&path)).unwrap();
            assert_eq!(normalized, [parent.canonicalize().unwrap().join("file")]);
            assert!(normalize_paths(&[path.clone(), normalized[0].clone()]).is_err());
            std::fs::rename(&path, dir.path().join("simulated-trash")).unwrap();
            std::fs::remove_file(&alias).unwrap();
            // Receipt validation must not re-resolve the caller's now-missing alias.
            assert_normalized_binding(path, normalized);
        }

        #[cfg(target_os = "windows")]
        #[test]
        fn canonical_windows_paths_use_shell_spelling() {
            for (canonical, shell) in [
                (r"\\?\C:\parent\file", r"C:\parent\file"),
                (r"\\?\UNC\host\share\file", r"\\host\share\file"),
            ] {
                assert_eq!(shell_path(canonical.into()), PathBuf::from(shell));
            }
        }

        #[test]
        fn same_path_receipts_restore_exact_ids_in_either_order() {
            let path = Path::new("parent/file");
            let first = item("first-id", path);
            let second = item("second-id", path);
            let receipt1 = bind_receipt(path, vec![], vec![first.clone()]).unwrap();
            for after in [
                vec![first.clone(), second.clone()],
                vec![second.clone(), first.clone()],
            ] {
                let receipt2 = bind_receipt(path, vec![first.clone()], after.clone()).unwrap();
                assert_eq!(receipt2.trash_id.as_ref(), Some(&second.id));
                for receipt in [&receipt1, &receipt2] {
                    let selected =
                        select_restore_items(after.clone(), std::slice::from_ref(receipt)).unwrap();
                    assert_eq!(selected.len(), 1);
                    assert_eq!(Some(&selected[0].id), receipt.trash_id.as_ref());
                }
            }
        }

        #[test]
        fn batch_binding_preserves_selection_order_not_trash_order() {
            let paths = [PathBuf::from("parent/a"), PathBuf::from("parent/b")];
            let old = item("old-a", &paths[0]);
            let first = item("new-a", &paths[0]);
            let second = item("new-b", &paths[1]);
            let unrelated = item("unrelated", Path::new("parent/other"));
            for after in [
                vec![
                    second.clone(),
                    old.clone(),
                    unrelated.clone(),
                    first.clone(),
                ],
                vec![first.clone(), unrelated, old.clone(), second.clone()],
            ] {
                for selection in [paths.clone(), [paths[1].clone(), paths[0].clone()]] {
                    let receipts =
                        bind_receipts(&selection, &selection, vec![old.clone()], after.clone())
                            .unwrap();
                    assert_eq!(receipts.len(), 2);
                    for (receipt, path) in receipts.iter().zip(&selection) {
                        assert_eq!(&receipt.original_path, path);
                        let expected = if path == &paths[0] {
                            &first.id
                        } else {
                            &second.id
                        };
                        assert_eq!(receipt.trash_id.as_ref(), Some(expected));
                    }
                    let restored = select_restore_items(after.clone(), &receipts).unwrap();
                    assert_eq!(restored.len(), 2);
                    for (item, receipt) in restored.iter().zip(receipts) {
                        assert_eq!(Some(item.id.clone()), receipt.trash_id);
                    }
                }
            }
        }

        #[test]
        fn batch_binding_rejects_incomplete_or_ambiguous_selection() {
            let paths = [PathBuf::from("parent/a"), PathBuf::from("parent/b")];
            let first = item("new-a", &paths[0]);
            let second = item("new-b", &paths[1]);
            for after in [
                vec![first.clone()], // One valid receipt cannot hide a missing item.
                vec![first.clone(), second.clone(), item("extra-b", &paths[1])],
                vec![first.clone(), second.clone(), second.clone()],
                vec![first.clone(), item("new-a", &paths[1])], // IDs must be unique too.
            ] {
                assert!(bind_receipts(&paths, &paths, vec![], after).is_err());
            }
            assert!(bind_receipts(
                &paths,
                &[paths[0].clone(), paths[0].clone()],
                vec![],
                vec![first]
            )
            .is_err());
        }

        #[test]
        fn binding_rejects_missing_or_ambiguous_new_ids() {
            let path = Path::new("parent/file");
            let old = item("old-id", path);
            let new = item("new-id", path);
            let other = item("other-id", Path::new("parent/other"));
            for after in [
                vec![],
                vec![old.clone(), other],
                vec![new.clone(), item("another-new-id", path)],
                vec![new.clone(), new.clone()],
            ] {
                assert!(bind_receipt(path, vec![old.clone()], after).is_err());
            }
            // An old ID must not become new just because its original path changed.
            let moved = item("old-id", Path::new("parent/other"));
            assert!(bind_receipt(path, vec![moved], vec![old]).is_err());
        }

        #[test]
        fn restore_rejects_missing_ambiguous_or_mismatched_ids_without_path_fallback() {
            let path = Path::new("parent/file");
            let target = item("target-id", path);
            let receipt = bind_receipt(path, vec![], vec![target.clone()]).unwrap();
            for items in [
                vec![],
                vec![item("other-id", path)],
                vec![target.clone(), target.clone()],
                vec![
                    item("target-id", Path::new("parent/other")),
                    item("other-id", path),
                ],
            ] {
                assert!(select_restore_items(items, std::slice::from_ref(&receipt)).is_err());
            }
            let missing_id = TrashReceipt {
                trash_id: None,
                ..receipt.clone()
            };
            assert!(select_restore_items(vec![target.clone()], &[missing_id]).is_err());
            let missing_path = TrashReceipt {
                normalized_path: None,
                ..receipt.clone()
            };
            assert!(select_restore_items(vec![target.clone()], &[missing_path]).is_err());
            // Duplicate receipt IDs cannot restore the same item twice.
            assert!(select_restore_items(vec![target], &[receipt.clone(), receipt]).is_err());
        }
    }
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
