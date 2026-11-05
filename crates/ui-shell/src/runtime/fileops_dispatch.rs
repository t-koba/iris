use super::*;

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
        let old_path = PathBuf::from(&target.display_path);
        let new_path = old_path
            .parent()
            .map(|parent| parent.join(new_name))
            .ok_or_else(|| format!("{} has no parent", target.display_path))?;
        let resolved = self.resolve_backend(&old_path);
        if resolved.kind == VolumeKind::Local && !self.is_virtual_display_path(&target.display_path)
        {
            if tokio::fs::try_exists(&new_path)
                .await
                .map_err(|error| error.to_string())?
            {
                return Err(format!(
                    "destination already exists: {}",
                    new_path.to_string_lossy()
                ));
            }
            tokio::fs::rename(&old_path, &new_path)
                .await
                .map_err(|error| error.to_string())?;
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
                    Ok(Some(MetadataResult {
                        level: MetadataLevel::L1,
                        is_dir: metadata.is_dir(),
                        size: if metadata.is_file() {
                            Some(metadata.len())
                        } else {
                            None
                        },
                        mtime: metadata.modified().ok(),
                        ctime: metadata.created().ok(),
                        content_type: mime_guess::from_path(path)
                            .first()
                            .map(|mime| mime.to_string()),
                        etag: None,
                    }))
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

    pub(super) async fn resolve_destination_path_async(
        &self,
        requested_dst: &Path,
        resolution: &ConflictResolution,
    ) -> Result<PathBuf, String> {
        if !self.path_exists(requested_dst).await? {
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
                let stem = requested_dst
                    .file_stem()
                    .map(|segment| segment.to_string_lossy().to_string())
                    .unwrap_or_default();
                let ext = requested_dst
                    .extension()
                    .map(|segment| format!(".{}", segment.to_string_lossy()))
                    .unwrap_or_default();
                let mut index = 1u32;
                loop {
                    let candidate =
                        requested_dst.with_file_name(format!("{} ({}){}", stem, index, ext));
                    if !self.path_exists(&candidate).await? {
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
    ) -> Result<Vec<iris_core_domain::types::list::ListEntry>, String> {
        if self.negative_cache_contains(
            &resolved.backend_id,
            &dir.display_path,
            NegativeCacheKind::EmptyDirectory,
        ) {
            return Ok(Vec::new());
        }
        if resolved.kind == VolumeKind::Local && !self.is_virtual_display_path(&dir.display_path) {
            let mut read_dir = tokio::fs::read_dir(&dir.display_path)
                .await
                .map_err(|error| error.to_string())?;
            let mut entries = Vec::new();
            while let Some(entry) = read_dir
                .next_entry()
                .await
                .map_err(|error| error.to_string())?
            {
                let path = entry.path();
                let full_path = path.to_string_lossy().to_string();
                let name = entry.file_name().to_string_lossy().to_string();

                let link_meta = match tokio::fs::symlink_metadata(&path).await {
                    Ok(m) => m,
                    Err(_) => {
                        entries.push(iris_core_domain::types::list::ListEntry {
                            object_ref: ObjectRef {
                                key: ObjectKey {
                                    backend_id: resolved.backend_id.clone(),
                                    stable_id: StableId(full_path.clone()),
                                },
                                parent_stable_id: Some(dir.key.stable_id.clone()),
                                display_path: full_path,
                            },
                            sort_name_key:
                                iris_core_domain::types::list::ListEntry::sort_name_key_for(&name),
                            sort_extension_key:
                                iris_core_domain::types::list::ListEntry::sort_extension_key_for(
                                    &name,
                                ),
                            name,
                            is_dir: false,
                            metadata_level: iris_core_domain::types::metadata::MetadataLevel::L0,
                            size: None,
                            mtime: None,
                            ctime: None,
                            is_symlink: false,
                            symlink_target: None,
                            is_inaccessible: true,
                            annotation: None,
                        });
                        continue;
                    }
                };

                let is_symlink = link_meta.is_symlink();
                let symlink_target = if is_symlink {
                    tokio::fs::read_link(&path)
                        .await
                        .ok()
                        .map(|t| t.to_string_lossy().to_string())
                } else {
                    None
                };
                let metadata = if is_symlink {
                    tokio::fs::metadata(&path).await.unwrap_or(link_meta)
                } else {
                    link_meta
                };
                entries.push(iris_core_domain::types::list::ListEntry {
                    object_ref: ObjectRef {
                        key: ObjectKey {
                            backend_id: resolved.backend_id.clone(),
                            stable_id: get_local_stable_id(&path)
                                .unwrap_or_else(|_| StableId(full_path.clone())),
                        },
                        parent_stable_id: Some(dir.key.stable_id.clone()),
                        display_path: full_path,
                    },
                    sort_name_key: iris_core_domain::types::list::ListEntry::sort_name_key_for(
                        &name,
                    ),
                    sort_extension_key:
                        iris_core_domain::types::list::ListEntry::sort_extension_key_for(&name),
                    name,
                    is_dir: metadata.is_dir(),
                    metadata_level: iris_core_domain::types::metadata::MetadataLevel::L1,
                    size: if metadata.is_file() {
                        Some(metadata.len())
                    } else {
                        None
                    },
                    mtime: metadata.modified().ok(),
                    ctime: metadata.created().ok(),
                    is_symlink,
                    symlink_target,
                    is_inaccessible: false,
                    annotation: None,
                });
            }
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
            return Ok(entries);
        }

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
                let mut entry =
                    iris_core_domain::types::list::ListEntry::from_listed_entry(listed_entry);
                self.remember_remote_path(
                    &resolved.backend_id,
                    &entry.object_ref.display_path,
                    &entry.object_ref.key.stable_id,
                );
                entry.object_ref.display_path = self
                    .absolute_display_path(&resolved.backend_id, &entry.object_ref.display_path);
                entries.push(entry);
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn rename_object_refuses_existing_local_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_path = temp.path().join("src.txt");
        let dst_path = temp.path().join("dst.txt");
        fs::write(&src_path, b"source").expect("write source");
        fs::write(&dst_path, b"existing").expect("write destination");

        let source = ObjectRef {
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
