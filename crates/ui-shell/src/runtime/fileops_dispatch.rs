use super::*;
use iris_backends::localfs::adapter::metadata_result;
use iris_backends::localfs::stable_id::get_stable_id_from_metadata;
use iris_core_domain::types::list::ListEntry;

impl AppRuntime {
    pub async fn open_object(&self, object: &ObjectRef) -> Result<(), String> {
        let local_path = self.materialize_object_for_open(object).await?;
        default_app::open_with_default_app(&local_path).map_err(|error| error.to_string())
    }

    pub async fn create_folder(&self, directory_path: &str, name: &str) -> Result<(), String> {
        let parent_path = Path::new(directory_path);
        let resolved = self.resolve_backend(parent_path);
        let created_path = parent_path.join(name);
        if resolved.kind == VolumeKind::Local && !self.is_virtual_display_path(directory_path) {
            tokio::fs::create_dir(&created_path)
                .await
                .map_err(|error| error.to_string())?;
            let created_display_path = created_path.to_string_lossy();
            self.clear_negative_path(&resolved.backend_id, &created_display_path);
            self.clear_negative_path(&resolved.backend_id, directory_path);
            return Ok(());
        }

        let parent = self.object_ref_for_display_path(parent_path, true)?;
        resolved
            .adapter
            .mkdir(&parent, name)
            .await
            .map_err(user_facing_backend_error)?;
        let created_display_path = created_path.to_string_lossy();
        self.clear_negative_path(&resolved.backend_id, &created_display_path);
        self.clear_negative_path(&resolved.backend_id, directory_path);
        Ok(())
    }

    pub async fn rename_object(&self, target: &ObjectRef, new_name: &str) -> Result<(), String> {
        target
            .ensure_operable()
            .map_err(|error| error.to_string())?;
        let old_path = PathBuf::from(&target.display_path);
        let new_path = old_path
            .parent()
            .map(|parent| parent.join(new_name))
            .ok_or_else(|| format!("{} has no parent", target.display_path))?;
        let resolved = self.resolve_backend(&old_path);
        if resolved.kind == VolumeKind::Local && !self.is_virtual_display_path(&target.display_path)
        {
            let (src, dst) = (old_path.clone(), new_path.clone());
            let renamed = tokio::task::spawn_blocking(move || {
                iris_backends::common::rename_no_replace(&src, &dst)
            })
            .await
            .map_err(|error| error.to_string())?;
            if let Err(error) = renamed {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    return Err(format!(
                        "destination already exists: {}",
                        new_path.to_string_lossy()
                    ));
                }
                return Err(error.to_string());
            }
            self.rebind_cached_display_path_prefix(
                &resolved.backend_id,
                &target.display_path,
                &new_path.to_string_lossy(),
            );
            return Ok(());
        }

        if !resolved.adapter.capabilities().can_rename {
            return Err("this backend does not allow rename without verify support".to_string());
        }

        let src = self.source_object_ref(target, &resolved)?;
        let dst = self.object_ref_for_display_path(&new_path, false)?;
        resolved
            .adapter
            .move_object(
                &src,
                &dst,
                MoveOptions {
                    on_conflict: ConflictResolution::Fail,
                },
            )
            .await
            .map_err(user_facing_backend_error)?;
        self.rebind_cached_display_path_prefix(
            &resolved.backend_id,
            &target.display_path,
            &new_path.to_string_lossy(),
        );
        Ok(())
    }

    pub async fn delete_objects(
        &self,
        targets: &[ObjectRef],
        use_trash: bool,
    ) -> Result<(), String> {
        for target in targets {
            self.delete_object(target, use_trash).await?;
        }
        Ok(())
    }

    pub async fn conflict_metadata(
        &self,
        source: &ObjectRef,
        destination_path: &str,
    ) -> Result<Option<ConflictMetadata>, String> {
        let destination = match self.stat_path(destination_path).await? {
            Some(metadata) => metadata,
            None => return Ok(None),
        };
        let source_meta = self.stat_object(source).await?;
        Ok(Some(ConflictMetadata {
            source: source.clone(),
            destination_path: destination_path.to_string(),
            source_size: source_meta.size.unwrap_or(0),
            destination_size: destination.size.unwrap_or(0),
            source_mtime: source_meta.mtime,
            destination_mtime: destination.mtime,
        }))
    }

    pub(super) async fn stat_path(&self, path: &str) -> Result<Option<MetadataResult>, String> {
        let path = Path::new(path);
        let resolved = self.resolve_backend(path);
        let display_path = path.to_string_lossy();
        if self.negative_cache_contains(
            &resolved.backend_id,
            &display_path,
            NegativeCacheKind::MissingPath,
        ) {
            return Ok(None);
        }
        if resolved.kind == VolumeKind::Local
            && !self.is_virtual_display_path(display_path.as_ref())
        {
            return match tokio::fs::metadata(path).await {
                Ok(metadata) => {
                    self.clear_negative_path(&resolved.backend_id, &display_path);
                    Ok(Some(metadata_result(path, &metadata, MetadataLevel::L1)))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.remember_negative_path(
                        &resolved.backend_id,
                        &display_path,
                        NegativeCacheKind::MissingPath,
                    );
                    Ok(None)
                }
                Err(error) => Err(error.to_string()),
            };
        }

        let object = self.object_ref_for_path(path, &resolved)?;
        if object.key.stable_id.0.starts_with("pending:") {
            self.remember_negative_path(
                &resolved.backend_id,
                &display_path,
                NegativeCacheKind::MissingPath,
            );
            return Ok(None);
        }
        match resolved.adapter.stat(&object).await {
            Ok(metadata) => {
                self.clear_negative_path(&resolved.backend_id, &display_path);
                Ok(Some(metadata))
            }
            Err(BackendError::NotFound { .. }) => {
                self.remember_negative_path(
                    &resolved.backend_id,
                    &display_path,
                    NegativeCacheKind::MissingPath,
                );
                Ok(None)
            }
            Err(error) => Err(user_facing_backend_error(error)),
        }
    }

    pub(super) async fn delete_object(
        &self,
        object: &ObjectRef,
        use_trash: bool,
    ) -> Result<(), String> {
        object
            .ensure_operable()
            .map_err(|error| error.to_string())?;
        let path = Path::new(&object.display_path);
        let resolved = self.resolve_backend(path);
        let object_ref = self.source_object_ref(object, &resolved)?;
        if self.is_directory_object(object).await? && resolved.kind != VolumeKind::Local {
            let mut cursor = None;
            loop {
                let page = resolved
                    .adapter
                    .list_names(&object_ref, cursor.clone())
                    .await
                    .map_err(user_facing_backend_error)?;
                for child in page.entries {
                    let child = child.name();
                    let child_object = ObjectRef {
                        display_path: self.absolute_display_path(
                            &resolved.backend_id,
                            &child.object_ref.display_path,
                        ),
                        ..child.object_ref.clone()
                    };
                    Box::pin(self.delete_object(&child_object, false)).await?;
                }
                cursor = page.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
        }
        resolved
            .adapter
            .delete(
                &object_ref,
                if use_trash {
                    DeleteMode::Trash
                } else {
                    DeleteMode::Permanent
                },
            )
            .await
            .map_err(user_facing_backend_error)?;
        self.purge_cached_display_path_prefix(&resolved.backend_id, &object.display_path);
        self.remember_negative_path(
            &resolved.backend_id,
            &object.display_path,
            NegativeCacheKind::MissingPath,
        );
        Ok(())
    }

    pub(super) async fn is_directory_object(&self, object: &ObjectRef) -> Result<bool, String> {
        object
            .ensure_operable()
            .map_err(|error| error.to_string())?;
        let path = Path::new(&object.display_path);
        let resolved = self.resolve_backend(path);
        if resolved.kind == VolumeKind::Local && !self.is_virtual_display_path(&object.display_path)
        {
            return tokio::fs::metadata(path)
                .await
                .map(|metadata| metadata.is_dir())
                .map_err(|error| error.to_string());
        }
        let object_ref = self.source_object_ref(object, &resolved)?;
        if object_ref.key.stable_id == resolved.root_stable_id {
            return Ok(true);
        }
        Ok(object_ref.key.stable_id.0.starts_with("folder:")
            || object_ref.display_path.ends_with('/'))
    }

    pub(super) async fn materialize_object_for_open(
        &self,
        object: &ObjectRef,
    ) -> Result<PathBuf, String> {
        object
            .ensure_operable()
            .map_err(|error| error.to_string())?;
        let path = Path::new(&object.display_path);
        let resolved = self.resolve_backend(path);
        if resolved.kind == VolumeKind::Local && !self.is_virtual_display_path(&object.display_path)
        {
            return Ok(path.to_path_buf());
        }

        let source = self.source_object_ref(object, &resolved)?;
        let mut handle = resolved
            .adapter
            .read(&source, None)
            .await
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        handle
            .stream
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| error.to_string())?;

        let mut local_path = self.open_cache_dir.join(sanitize_file_name(
            Path::new(&object.display_path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("download"),
        ));
        if local_path.exists() {
            local_path = unique_sibling_path(&local_path);
        }
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(&local_path, bytes).map_err(|error| error.to_string())?;
        Ok(local_path)
    }

    pub(super) async fn path_exists(&self, path: &Path) -> Result<bool, String> {
        Ok(self
            .stat_path(path.to_string_lossy().as_ref())
            .await?
            .is_some())
    }

    /// Destination occupancy is about the entry, not its target. Local probes bypass
    /// metadata/negative caches so a dangling link (including one just created) is occupied.
    pub(super) async fn destination_entry_exists(&self, path: &Path) -> Result<bool, String> {
        let resolved = self.resolve_backend(path);
        if resolved.kind == VolumeKind::Local
            && !self.is_virtual_display_path(path.to_string_lossy().as_ref())
        {
            return directory_entry_exists(tokio::fs::symlink_metadata(path).await);
        }
        self.path_exists(path).await
    }

    pub(super) async fn resolve_destination_path_async(
        &self,
        requested_dst: &Path,
        resolution: &ConflictResolution,
    ) -> Result<PathBuf, String> {
        if !self.destination_entry_exists(requested_dst).await? {
            return Ok(requested_dst.to_path_buf());
        }
        if !self.is_virtual_display_path(requested_dst.to_string_lossy().as_ref()) {
            return resolve_destination_path(requested_dst, resolution);
        }

        match resolution {
            ConflictResolution::Overwrite | ConflictResolution::Skip => {
                Ok(requested_dst.to_path_buf())
            }
            ConflictResolution::Fail => Err(format!(
                "destination already exists: {}",
                requested_dst.display()
            )),
            ConflictResolution::Rename => {
                let mut index = 1u64;
                loop {
                    let candidate = numbered_sibling_path(requested_dst, index);
                    if !self.destination_entry_exists(&candidate).await? {
                        return Ok(candidate);
                    }
                    index += 1;
                }
            }
        }
    }

    pub(super) async fn list_directory_children(
        &self,
        dir: &ObjectRef,
        resolved: &ResolvedBackend,
    ) -> Result<Vec<ListEntry>, String> {
        dir.ensure_operable().map_err(|error| error.to_string())?;
        if self.negative_cache_contains(
            &resolved.backend_id,
            &dir.display_path,
            NegativeCacheKind::EmptyDirectory,
        ) {
            return Ok(Vec::new());
        }
        let entries = if resolved.kind == VolumeKind::Local
            && !self.is_virtual_display_path(&dir.display_path)
        {
            let mut read_dir = tokio::fs::read_dir(&dir.display_path)
                .await
                .map_err(|error| error.to_string())?;
            let mut entries = Vec::new();
            while let Some(entry) = read_dir
                .next_entry()
                .await
                .map_err(|error| error.to_string())?
            {
                let name = entry.file_name().to_string_lossy().to_string();
                entries.push(
                    local_list_entry(entry.path(), name, &resolved.backend_id, &dir.key.stable_id)
                        .await,
                );
            }
            entries
        } else {
            let source_dir = self.source_object_ref(dir, resolved)?;
            let mut entries = Vec::new();
            let mut cursor = None;
            loop {
                let page = resolved
                    .adapter
                    .list_names(&source_dir, cursor.clone())
                    .await
                    .map_err(|error| error.to_string())?;
                for listed_entry in page.entries {
                    let mut entry = ListEntry::from_listed_entry(listed_entry);
                    self.remember_remote_path(
                        &resolved.backend_id,
                        &entry.object_ref.display_path,
                        &entry.object_ref.key.stable_id,
                    );
                    entry.object_ref.display_path = self.absolute_display_path(
                        &resolved.backend_id,
                        &entry.object_ref.display_path,
                    );
                    entries.push(entry);
                }
                cursor = page.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            entries
        };
        if entries.is_empty() {
            self.remember_negative_path(
                &resolved.backend_id,
                &dir.display_path,
                NegativeCacheKind::EmptyDirectory,
            );
        } else {
            self.clear_negative_path(&resolved.backend_id, &dir.display_path);
        }
        self.record_directory_snapshot(
            &resolved.backend_id,
            &dir.key.stable_id,
            entries.len(),
            true,
        );
        Ok(entries)
    }
}

/// An L1 entry for a local path; when metadata cannot be read it is marked `is_inaccessible`.
pub(crate) async fn local_list_entry(
    full_path: PathBuf,
    name: String,
    backend_id: &BackendId,
    parent_stable_id: &StableId,
) -> ListEntry {
    let display_path = full_path.to_string_lossy().to_string();

    // symlink_metadata describes the link itself and usually succeeds even without permission.
    let link_meta = tokio::fs::symlink_metadata(&full_path).await;

    match link_meta {
        Ok(lm) => {
            let is_symlink = lm.is_symlink();
            let symlink_target = if is_symlink {
                tokio::fs::read_link(&full_path)
                    .await
                    .ok()
                    .map(|t| t.to_string_lossy().to_string())
            } else {
                None
            };
            // Use the link itself for broken links or unreadable targets.
            let metadata = if is_symlink {
                tokio::fs::metadata(&full_path).await.unwrap_or(lm)
            } else {
                lm
            };

            let is_dir = metadata.is_dir();
            let size = if is_dir { None } else { Some(metadata.len()) };
            let mtime = metadata.modified().ok();
            let ctime = metadata.created().ok();
            let stable_id = if full_path.to_str().is_none() {
                // Even hard-linked native names with the same display label stay distinct.
                StableId(iris_platform_integration::staging::encode_path(&full_path))
            } else {
                get_stable_id_from_metadata(&full_path, &metadata).unwrap_or_else(|_| {
                    StableId(iris_platform_integration::staging::encode_path(&full_path))
                })
            };
            let object_ref = ObjectRef {
                local_path_is_lossy: full_path.to_str().is_none(),
                key: ObjectKey {
                    backend_id: backend_id.clone(),
                    stable_id,
                },
                parent_stable_id: Some(parent_stable_id.clone()),
                display_path,
            };
            ListEntry {
                metadata_level: MetadataLevel::L1,
                size,
                mtime,
                ctime,
                is_symlink,
                symlink_target,
                ..ListEntry::new(object_ref, name, is_dir)
            }
        }
        Err(_) => {
            // Metadata could not be read (e.g. permission denied).
            let stable_id = StableId(iris_platform_integration::staging::encode_path(&full_path));
            ListEntry {
                is_inaccessible: true,
                ..ListEntry::new(
                    ObjectRef {
                        local_path_is_lossy: full_path.to_str().is_none(),
                        key: ObjectKey {
                            backend_id: backend_id.clone(),
                            stable_id,
                        },
                        parent_stable_id: Some(parent_stable_id.clone()),
                        display_path,
                    },
                    name,
                    false,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(all(unix, not(target_os = "macos")))]
    #[tokio::test]
    async fn native_name_ui_listing_cache_and_operations_refuse_lossy_alias() {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir().unwrap();
        let native = temp
            .path()
            .join(std::ffi::OsString::from_vec(b"native-\xff.txt".to_vec()));
        let alias = temp.path().join("native-\u{fffd}.txt");
        fs::write(&native, b"A").unwrap();
        fs::write(&alias, b"replacement sibling B").unwrap();
        let runtime = app_runtime();
        let root = temp.path().to_str().unwrap();
        // Exercise the progressive L0 listing and its persistent directory snapshot.
        let entries = crate::update::helpers::read_directory(root.into(), true)
            .await
            .unwrap();
        let cached = runtime.cached_directory_entries(root, true);
        for entries in [&entries, &cached] {
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].name, entries[1].name);
            assert_ne!(entries[0].object_ref.key, entries[1].object_ref.key);
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| entry.object_ref.local_path_is_lossy)
                    .count(),
                1
            );
        }
        // L1 runtime enumeration must preserve the same refusal, not just the L0 path.
        let l1 = local_list_entry(
            native.clone(),
            "native-\u{fffd}.txt".into(),
            &runtime.backend_id_for_path(temp.path()),
            &runtime.stable_id_for_path(temp.path()),
        )
        .await;
        assert!(l1.object_ref.local_path_is_lossy);
        for object in [
            l1.object_ref,
            cached
                .iter()
                .find(|entry| entry.object_ref.local_path_is_lossy)
                .unwrap()
                .object_ref
                .clone(),
        ] {
            let expected = "local path cannot be represented losslessly as Unicode";
            assert!(runtime
                .stat_object(&object)
                .await
                .unwrap_err()
                .contains(expected));
            let updates = runtime.stat_objects(vec![object.clone()]).await;
            assert!(updates[0].1.as_ref().unwrap_err().contains(expected));
            assert!(runtime
                .materialize_object_for_open(&object)
                .await
                .unwrap_err()
                .contains(expected));
            assert!(runtime
                .rename_object(&object, "renamed.txt")
                .await
                .unwrap_err()
                .contains(expected));
            assert!(runtime
                .delete_objects(std::slice::from_ref(&object), false)
                .await
                .unwrap_err()
                .contains(expected));
            assert!(runtime
                .ensure_safe_transfer_destination(&object, &temp.path().join("copy.txt"))
                .await
                .unwrap_err()
                .contains(expected));
            for is_move in [false, true] {
                let error = Arc::clone(runtime)
                    .apply_pending_item(
                        JobId(9_900_002),
                        0,
                        PendingFileItem {
                            source: object.clone(),
                            destination_path: temp.path().join("copy.txt").to_str().unwrap().into(),
                            installed_path: None,
                            undoable: true,
                        },
                        is_move,
                        ConflictResolution::Overwrite,
                        0,
                    )
                    .await
                    .unwrap_err();
                assert!(error.contains(expected));
            }
            let id = JobId(9_900_001);
            assert!(runtime
                .persist_pending_job(
                    &id,
                    "native path refusal",
                    &[PendingFileItem {
                        source: object,
                        destination_path: temp.path().join("copy.txt").to_str().unwrap().into(),
                        installed_path: None,
                        undoable: true,
                    }],
                    false
                )
                .unwrap_err()
                .contains(expected));
            assert!(runtime.job_store.load_job(&id).unwrap().is_none());
        }
        assert!(!temp.path().join("renamed.txt").exists());
        assert!(!temp.path().join("copy.txt").exists());
        assert_eq!(fs::read(native).unwrap(), b"A");
        assert_eq!(fs::read(alias).unwrap(), b"replacement sibling B");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rename_object_refuses_dangling_local_symlink_destination() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let src_path = temp.path().join("src.txt");
        let dst_path = temp.path().join("dangling");
        let missing_target = temp.path().join("missing-target");
        fs::write(&src_path, b"source").expect("write source");
        symlink(&missing_target, &dst_path).expect("create dangling symlink");

        let source = ObjectRef {
            local_path_is_lossy: false,
            key: ObjectKey {
                backend_id: BackendId("local".to_string()),
                stable_id: StableId("src".to_string()),
            },
            parent_stable_id: Some(StableId("root".to_string())),
            display_path: src_path.to_string_lossy().to_string(),
        };

        let error = app_runtime()
            .rename_object(&source, "dangling")
            .await
            .expect_err("rename must not replace a dangling symlink");
        assert!(error.contains("destination already exists"));
        assert_eq!(fs::read(&src_path).expect("read source"), b"source");
        assert_eq!(
            fs::read_link(&dst_path).expect("read dangling symlink"),
            missing_target
        );

        app_runtime()
            .rename_object(&source, "renamed.txt")
            .await
            .expect("rename to an unused sibling should succeed");
        assert_eq!(
            fs::read(temp.path().join("renamed.txt")).expect("read renamed source"),
            b"source"
        );
    }

    #[tokio::test]
    async fn rename_object_refuses_existing_local_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_path = temp.path().join("src.txt");
        let dst_path = temp.path().join("dst.txt");
        fs::write(&src_path, b"source").expect("write source");
        fs::write(&dst_path, b"existing").expect("write destination");

        let source = ObjectRef {
            local_path_is_lossy: false,
            key: ObjectKey {
                backend_id: BackendId("local".to_string()),
                stable_id: StableId("src".to_string()),
            },
            parent_stable_id: Some(StableId("root".to_string())),
            display_path: src_path.to_string_lossy().to_string(),
        };

        let error = app_runtime()
            .rename_object(&source, "dst.txt")
            .await
            .expect_err("rename should fail on conflict");

        assert!(error.contains("destination already exists"));
        assert_eq!(fs::read(&src_path).expect("read source"), b"source");
        assert_eq!(fs::read(&dst_path).expect("read destination"), b"existing");
    }
}
