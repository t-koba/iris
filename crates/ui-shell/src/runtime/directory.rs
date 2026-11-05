use super::*;
use tokio::task::JoinSet;

const STAT_OBJECT_CONCURRENCY: usize = 16;

/// `/Users/me` -> `/`, `Users`, `me`; `C:\\Users` -> `C:`, `Users` (the drive and its root are
/// one crumb).
fn local_breadcrumbs(path: &Path) -> Vec<(String, String)> {
    let mut segments: Vec<(String, String)> = Vec::new();
    let mut accumulated = PathBuf::new();
    for component in path.components() {
        accumulated.push(component);
        let full_path = accumulated.to_string_lossy().into_owned();
        match component {
            std::path::Component::Prefix(prefix) => {
                segments.push((prefix.as_os_str().to_string_lossy().into_owned(), full_path));
            }
            std::path::Component::RootDir => match segments.last_mut() {
                Some(drive) => drive.1 = full_path,
                None => segments.push(("/".to_string(), full_path)),
            },
            std::path::Component::Normal(segment) => {
                segments.push((segment.to_string_lossy().into_owned(), full_path));
            }
            std::path::Component::CurDir | std::path::Component::ParentDir => {}
        }
    }
    segments
}

impl AppRuntime {
    pub(crate) fn cached_directory_entries(
        &self,
        path: &str,
        show_hidden: bool,
    ) -> Vec<iris_core_domain::types::list::ListEntry> {
        let backend_id = self.backend_id_for_path(Path::new(path));
        let dir_stable_id = self.stable_id_for_path(Path::new(path));
        if dir_stable_id.0.starts_with("pending:") {
            return Vec::new();
        }
        let cached = self
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_snapshot_entries(&backend_id.0, &dir_stable_id.0)
            .ok()
            .flatten()
            .unwrap_or_default();
        cached
            .into_iter()
            .filter(|entry| show_hidden || !entry.name.starts_with('.'))
            .map(|entry| iris_core_domain::types::list::ListEntry {
                object_ref: ObjectRef {
                    key: ObjectKey {
                        backend_id: backend_id.clone(),
                        stable_id: StableId(entry.stable_id),
                    },
                    parent_stable_id: Some(dir_stable_id.clone()),
                    display_path: entry.display_path,
                },
                sort_name_key: iris_core_domain::types::list::ListEntry::sort_name_key_for(
                    &entry.name,
                ),
                sort_extension_key:
                    iris_core_domain::types::list::ListEntry::sort_extension_key_for(&entry.name),
                name: entry.name,
                is_dir: entry.is_dir,
                metadata_level: entry.metadata_level,
                size: entry.size,
                mtime: entry.mtime,
                ctime: entry.ctime,
                is_symlink: entry.is_symlink,
                symlink_target: entry.symlink_target,
                is_inaccessible: entry.is_inaccessible,
                annotation: None,
            })
            .collect()
    }

    pub(crate) fn begin_directory_snapshot_for_path(
        &self,
        path: &str,
    ) -> Option<DirectorySnapshotWrite> {
        let backend_id = self.backend_id_for_path(Path::new(path));
        let dir_stable_id = self.stable_id_for_path(Path::new(path));
        if dir_stable_id.0.starts_with("pending:") {
            return None;
        }
        self.begin_directory_snapshot(&backend_id, &dir_stable_id)
    }

    fn begin_directory_snapshot(
        &self,
        backend_id: &BackendId,
        dir_stable_id: &StableId,
    ) -> Option<DirectorySnapshotWrite> {
        let generation = match self
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .begin_snapshot_generation(&backend_id.0, &dir_stable_id.0)
        {
            Ok(generation) => generation,
            Err(error) => {
                tracing::warn!(
                    backend_id = backend_id.0,
                    dir_stable_id_hash = %iris_core_domain::observability::path_hash(&dir_stable_id.0),
                    "failed to begin directory snapshot generation: {error}"
                );
                return None;
            }
        };
        Some(DirectorySnapshotWrite {
            backend_id: backend_id.clone(),
            dir_stable_id: dir_stable_id.clone(),
            generation,
        })
    }

    pub(crate) fn append_directory_snapshot_entries(
        &self,
        write: &DirectorySnapshotWrite,
        entries: &[iris_core_domain::types::list::ListEntry],
    ) -> bool {
        let cached = entries
            .iter()
            .map(|entry| CachedDirectoryEntry {
                stable_id: entry.object_ref.key.stable_id.0.clone(),
                display_path: entry.object_ref.display_path.clone(),
                name: entry.name.clone(),
                is_dir: entry.is_dir,
                metadata_level: entry.metadata_level,
                size: entry.size,
                mtime: entry.mtime,
                ctime: entry.ctime,
                is_symlink: entry.is_symlink,
                symlink_target: entry.symlink_target.clone(),
                is_inaccessible: entry.is_inaccessible,
            })
            .collect::<Vec<_>>();
        self.catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .append_snapshot_entries(
                &write.backend_id.0,
                &write.dir_stable_id.0,
                write.generation,
                &cached,
            )
            .is_ok()
    }

    pub(crate) fn complete_directory_snapshot(&self, write: DirectorySnapshotWrite) -> bool {
        self.catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .complete_snapshot_generation(
                &write.backend_id.0,
                &write.dir_stable_id.0,
                write.generation,
            )
            .is_ok()
    }

    pub fn backend_id_for_path(&self, path: &Path) -> BackendId {
        self.resolve_backend(path).backend_id
    }

    pub fn is_virtual_display_path(&self, path: &str) -> bool {
        self.parse_virtual_display_path(path).is_some()
    }

    pub fn format_display_path(&self, path: &str) -> String {
        if let Some((backend_id, relative_path)) = self.parse_virtual_display_path(path) {
            let backend_name = self
                .registered_backends
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .find(|backend| backend.backend_id == backend_id)
                .map(|backend| backend.display_name.clone())
                .unwrap_or_else(|| backend_id.0);
            if relative_path.is_empty() {
                backend_name
            } else {
                format!("{}/{}", backend_name, relative_path)
            }
        } else {
            path.to_string()
        }
    }

    pub fn resolve_navigation_input(&self, input: &str, current_path: &str) -> Option<String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Some((backend_id, relative_path)) = self.parse_virtual_display_path(trimmed) {
            return Some(self.absolute_display_path(
                &backend_id,
                &normalize_relative_display_path(&relative_path),
            ));
        }

        if let Some(expanded_home) = expand_home_path(trimmed) {
            return Some(normalize_local_path(Path::new(&expanded_home)));
        }

        if Path::new(trimmed).is_absolute() {
            return Some(normalize_local_path(Path::new(trimmed)));
        }

        if !self.is_virtual_display_path(current_path) {
            let local_candidate = normalize_local_path(&PathBuf::from(current_path).join(trimmed));
            if Path::new(&local_candidate).exists() {
                return Some(local_candidate);
            }
        }

        if let Some(virtual_path) = self.resolve_display_named_virtual_path(trimmed) {
            return Some(virtual_path);
        }

        if let Some((backend_id, current_relative)) = self.parse_virtual_display_path(current_path)
        {
            let joined = if current_relative.is_empty() {
                trimmed.to_string()
            } else {
                join_display_path(&current_relative, trimmed)
            };
            return Some(
                self.absolute_display_path(&backend_id, &normalize_relative_display_path(&joined)),
            );
        }

        Some(normalize_local_path(
            &PathBuf::from(current_path).join(trimmed),
        ))
    }

    pub fn path_completion_candidates(
        &self,
        input: &str,
        current_path: &str,
        limit: usize,
    ) -> Vec<String> {
        if limit == 0 {
            return Vec::new();
        }

        let trimmed = input.trim();
        let mut candidates = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let query = trimmed.to_lowercase();

        let registered_backends = self
            .registered_backends
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        for backend in registered_backends {
            if !query.is_empty()
                && backend.display_name.to_lowercase().contains(&query)
                && seen.insert(backend.display_name.clone())
            {
                candidates.push(backend.display_name);
                if candidates.len() >= limit {
                    return candidates;
                }
            }
        }

        let prefix = if trimmed.is_empty() {
            current_path.to_string()
        } else {
            self.resolve_navigation_input(trimmed, current_path)
                .unwrap_or_else(|| current_path.to_string())
        };
        let backend_id = self.backend_id_for_path(Path::new(&prefix));
        if let Ok(paths) = self
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .list_object_display_paths_by_prefix(&backend_id.0, &prefix, limit)
        {
            for path in paths {
                let display_path = self.format_display_path(&path);
                if seen.insert(display_path.clone()) {
                    candidates.push(display_path);
                    if candidates.len() >= limit {
                        return candidates;
                    }
                }
            }
        }

        if let Some((backend_id, _)) = self.parse_virtual_display_path(&prefix) {
            let known_paths = self
                .known_remote_paths
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .keys()
                .filter(|(id, _)| id == &backend_id.0)
                .map(|(_, relative)| relative.clone())
                .collect::<Vec<_>>();
            for relative in known_paths {
                let path = self.absolute_display_path(&backend_id, &relative);
                if !path.starts_with(&prefix) {
                    continue;
                }
                let display_path = self.format_display_path(&path);
                if seen.insert(display_path.clone()) {
                    candidates.push(display_path);
                    if candidates.len() >= limit {
                        return candidates;
                    }
                }
            }
        }

        candidates
    }

    pub fn breadcrumbs_for_display_path(&self, path: &str) -> Vec<(String, String)> {
        if let Some((backend_id, relative_path)) = self.parse_virtual_display_path(path) {
            let backend_name = self
                .registered_backends
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .find(|backend| backend.backend_id == backend_id)
                .map(|backend| backend.display_name.clone())
                .unwrap_or_else(|| backend_id.0.clone());
            let root_path = virtual_root_path(&backend_id).to_string_lossy().to_string();
            let mut segments = vec![(backend_name, root_path.clone())];
            let mut current = root_path;
            for part in relative_path.split('/').filter(|part| !part.is_empty()) {
                current = join_display_path(&current, part);
                segments.push((part.to_string(), current.clone()));
            }
            return segments;
        }

        local_breadcrumbs(Path::new(path))
    }

    pub async fn read_directory_entries(
        &self,
        path: &str,
        show_hidden: bool,
        limit: Option<usize>,
    ) -> Result<Vec<iris_core_domain::types::list::ListEntry>, String> {
        let resolved = self.resolve_backend(Path::new(path));
        if !self.is_virtual_display_path(path) {
            return Err(format!("{} is not a virtual backend path", path));
        }
        if self.negative_cache_contains(
            &resolved.backend_id,
            path,
            NegativeCacheKind::EmptyDirectory,
        ) {
            return Ok(Vec::new());
        }

        let dir = self.object_ref_for_display_path(Path::new(path), true)?;
        let mut snapshot_write =
            self.begin_directory_snapshot(&resolved.backend_id, &dir.key.stable_id);
        let mut entries = Vec::new();
        let mut cursor = None;
        let mut listed_count = 0usize;

        loop {
            let page = resolved
                .adapter
                .list_names(&dir, cursor.clone())
                .await
                .map_err(user_facing_backend_error)?;
            let mut snapshot_page = Vec::with_capacity(page.entries.len());
            let mut metadata_page = Vec::with_capacity(page.entries.len());
            for listed_entry in page.entries {
                listed_count += 1;
                let mut entry =
                    iris_core_domain::types::list::ListEntry::from_listed_entry(listed_entry);
                let full_path = self
                    .absolute_display_path(&resolved.backend_id, &entry.object_ref.display_path);
                self.remember_remote_path(
                    &resolved.backend_id,
                    &entry.object_ref.display_path,
                    &entry.object_ref.key.stable_id,
                );
                entry.object_ref.display_path = full_path.clone();
                if !entry.metadata_pending() {
                    metadata_page.push(CachedObject {
                        backend_id: entry.object_ref.key.backend_id.0.clone(),
                        stable_id: entry.object_ref.key.stable_id.0.clone(),
                        display_path: entry.object_ref.display_path.clone(),
                        size: if entry.is_dir { None } else { entry.size },
                        mtime: entry.mtime,
                        content_type: mime_guess::from_path(&entry.name)
                            .first()
                            .map(|mime| mime.to_string()),
                        etag: None,
                        cached_level: MetadataLevel::L1,
                        last_accessed: SystemTime::now(),
                    });
                }
                let hidden = entry.name.starts_with('.');
                snapshot_page.push(entry.clone());
                if !show_hidden && hidden {
                    continue;
                }
                entries.push(entry);
                if limit.is_some_and(|max| entries.len() >= max) {
                    if !metadata_page.is_empty() {
                        let _ = self
                            .catalog
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .upsert_objects(&metadata_page);
                    }
                    return Ok(entries);
                }
            }

            if !metadata_page.is_empty() {
                let _ = self
                    .catalog
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .upsert_objects(&metadata_page);
            }

            if snapshot_write
                .as_ref()
                .is_some_and(|write| !self.append_directory_snapshot_entries(write, &snapshot_page))
            {
                snapshot_write = None;
            }

            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

        if listed_count == 0 {
            self.remember_negative_path(
                &resolved.backend_id,
                path,
                NegativeCacheKind::EmptyDirectory,
            );
        } else {
            self.clear_negative_path(&resolved.backend_id, path);
        }
        if let Some(write) = snapshot_write {
            let _ = self.complete_directory_snapshot(write);
        }

        Ok(entries)
    }

    pub async fn stat_object(&self, object: &ObjectRef) -> Result<MetadataResult, String> {
        let path = Path::new(&object.display_path);
        let resolved = self.resolve_backend(path);
        if resolved.kind == VolumeKind::Local && !self.is_virtual_display_path(&object.display_path)
        {
            let metadata = tokio::fs::metadata(path)
                .await
                .map_err(|error| error.to_string())?;
            return Ok(MetadataResult {
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
            });
        }

        let source = self.source_object_ref(object, &resolved)?;
        resolved
            .adapter
            .stat(&source)
            .await
            .map_err(user_facing_backend_error)
    }

    pub async fn stat_objects(
        &self,
        objects: Vec<ObjectRef>,
    ) -> Vec<(ObjectRef, Result<MetadataResult, String>)> {
        if objects.is_empty() {
            return Vec::new();
        }

        let mut updates = Vec::with_capacity(objects.len());
        let mut local_jobs = Vec::new();
        let mut remote_jobs = Vec::new();

        for object in objects {
            let path = PathBuf::from(&object.display_path);
            let resolved = self.resolve_backend(&path);
            if resolved.kind == VolumeKind::Local
                && !self.is_virtual_display_path(&object.display_path)
            {
                local_jobs.push((object, path));
                continue;
            }

            match self.source_object_ref(&object, &resolved) {
                Ok(source) => remote_jobs.push((object, resolved.adapter.clone(), source)),
                Err(error) => updates.push((object, Err(error))),
            }
        }

        if !local_jobs.is_empty() {
            let fallback_objects = local_jobs
                .iter()
                .map(|(object, _)| object.clone())
                .collect::<Vec<_>>();
            match tokio::task::spawn_blocking(move || {
                local_jobs
                    .into_iter()
                    .map(|(object, path)| (object, stat_local_path_sync(&path)))
                    .collect::<Vec<_>>()
            })
            .await
            {
                Ok(results) => updates.extend(results),
                Err(error) => {
                    let message = error.to_string();
                    updates.extend(
                        fallback_objects
                            .into_iter()
                            .map(|object| (object, Err(message.clone()))),
                    );
                }
            }
        }

        let mut remote_iter = remote_jobs.into_iter();
        let mut tasks = JoinSet::new();
        for _ in 0..STAT_OBJECT_CONCURRENCY {
            let Some((object, adapter, source)) = remote_iter.next() else {
                break;
            };
            tasks.spawn(async move {
                let result = adapter
                    .stat(&source)
                    .await
                    .map_err(user_facing_backend_error);
                (object, result)
            });
        }

        while let Some(joined) = tasks.join_next().await {
            if let Ok(update) = joined {
                updates.push(update);
            }

            let Some((object, adapter, source)) = remote_iter.next() else {
                continue;
            };
            tasks.spawn(async move {
                let result = adapter
                    .stat(&source)
                    .await
                    .map_err(user_facing_backend_error);
                (object, result)
            });
        }

        updates
    }

    pub async fn read_object_prefix(
        &self,
        object: &ObjectRef,
        max_len: usize,
    ) -> Result<Vec<u8>, String> {
        let path = Path::new(&object.display_path);
        let resolved = self.resolve_backend(path);
        if resolved.kind == VolumeKind::Local && !self.is_virtual_display_path(&object.display_path)
        {
            return tokio::task::spawn_blocking({
                let path = path.to_path_buf();
                move || {
                    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
                    let mut buf = vec![0u8; max_len];
                    let read = file.read(&mut buf).map_err(|error| error.to_string())?;
                    buf.truncate(read);
                    Ok(buf)
                }
            })
            .await
            .map_err(|error| error.to_string())?;
        }

        let source = self.source_object_ref(object, &resolved)?;
        let mut handle = resolved
            .adapter
            .read(
                &source,
                Some(ByteRange {
                    offset: 0,
                    length: Some(max_len as u64),
                }),
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        handle
            .stream
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    pub async fn extract_image_metadata(
        &self,
        object: &ObjectRef,
    ) -> Result<ImageMetadata, String> {
        let path = Path::new(&object.display_path);
        let resolved = self.resolve_backend(path);
        if resolved.kind == VolumeKind::Local && path.exists() {
            let path = path.to_path_buf();
            return tokio::task::spawn_blocking(move || {
                Ok(iris_thumb_preview::metadata::extract_image_metadata_from_path(&path))
            })
            .await
            .map_err(|error| error.to_string())?;
        }

        let object_ref = self.source_object_ref(object, &resolved)?;
        iris_thumb_preview::metadata::extract_image_metadata_from_backend(
            resolved.adapter.clone(),
            object_ref,
        )
        .await
        .map_err(|error| error.to_string())
    }

    pub fn stable_id_for_path(&self, path: &Path) -> StableId {
        if let Some((backend_id, relative_path)) = self.parse_virtual_path(path) {
            if relative_path.is_empty() {
                return self
                    .registered_backends
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .iter()
                    .find(|backend| backend.backend_id == backend_id)
                    .map(|backend| backend.root_stable_id.clone())
                    .unwrap_or_else(|| StableId(format!("root:{}", backend_id.0)));
            }
            if let Some(stable_id) = self
                .known_remote_paths
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&(backend_id.0.clone(), relative_path.clone()))
                .cloned()
            {
                return stable_id;
            }
            return StableId(format!("pending:{}:{}", backend_id.0, relative_path));
        }
        get_local_stable_id(path).unwrap_or_else(|_| StableId(path.to_string_lossy().to_string()))
    }

    pub fn cache_list_entry_metadata(
        &self,
        object: &ObjectRef,
        size: Option<u64>,
        mtime: Option<SystemTime>,
        content_type: Option<String>,
    ) {
        if let Some((backend_id, relative_path)) =
            self.parse_virtual_display_path(&object.display_path)
        {
            self.known_remote_paths
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert((backend_id.0, relative_path), object.key.stable_id.clone());
        }

        let cached = CachedObject {
            backend_id: object.key.backend_id.0.clone(),
            stable_id: object.key.stable_id.0.clone(),
            display_path: object.display_path.clone(),
            size,
            mtime,
            content_type,
            etag: None,
            cached_level: MetadataLevel::L1,
            last_accessed: SystemTime::now(),
        };
        let _ = self
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .upsert_object(&cached);
    }

    pub(super) fn record_directory_snapshot(
        &self,
        backend_id: &BackendId,
        dir_stable_id: &StableId,
        item_count: usize,
        is_complete: bool,
    ) {
        let snapshot = DirectorySnapshot {
            backend_id: backend_id.0.clone(),
            dir_stable_id: dir_stable_id.0.clone(),
            generation: 0,
            item_count: item_count as u64,
            snapshot_at: SystemTime::now(),
            is_complete,
        };
        let _ = self
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .upsert_snapshot(&snapshot);
    }

    pub(crate) fn has_recent_complete_directory_snapshot(&self, path: &str) -> bool {
        let backend_id = self.backend_id_for_path(Path::new(path));
        let stable_id = self.stable_id_for_path(Path::new(path));
        if stable_id.0.starts_with("pending:") {
            return false;
        }

        let snapshot = self
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_snapshot(&backend_id.0, &stable_id.0)
            .ok()
            .flatten();
        let Some(snapshot) = snapshot else {
            return false;
        };
        snapshot.is_complete
            && snapshot
                .snapshot_at
                .elapsed()
                .is_ok_and(|age| age <= DIRECTORY_SNAPSHOT_FAST_PATH_TTL)
    }

    pub(super) fn rebind_cached_display_path_prefix(
        &self,
        backend_id: &BackendId,
        old_display_path: &str,
        new_display_path: &str,
    ) {
        self.clear_negative_path_prefix(backend_id, old_display_path);
        self.clear_negative_path_prefix(backend_id, new_display_path);
        self.remember_negative_path(backend_id, old_display_path, NegativeCacheKind::MissingPath);
        self.clear_parent_empty_cache(backend_id, old_display_path);
        self.clear_parent_empty_cache(backend_id, new_display_path);
        let _ = self
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .rebind_display_path_prefix(&backend_id.0, old_display_path, new_display_path);
        self.move_known_remote_path_prefix(backend_id, old_display_path, new_display_path);
    }

    pub(super) fn purge_cached_display_path_prefix(
        &self,
        backend_id: &BackendId,
        display_path: &str,
    ) {
        self.clear_negative_path_prefix(backend_id, display_path);
        self.clear_parent_empty_cache(backend_id, display_path);
        let removed_stable_ids = self
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .delete_objects_by_display_path_prefix(&backend_id.0, display_path)
            .unwrap_or_default();
        for stable_id in removed_stable_ids {
            let _ = self
                .session_cache
                .remove_prefix(&format!("thumb:{}:{}:", backend_id.0, stable_id));
        }
        self.forget_known_remote_path_prefix(backend_id, display_path);
    }

    pub(super) fn move_known_remote_path_prefix(
        &self,
        backend_id: &BackendId,
        old_display_path: &str,
        new_display_path: &str,
    ) {
        let Some((_, old_relative)) = self.parse_virtual_display_path(old_display_path) else {
            return;
        };
        let Some((_, new_relative)) = self.parse_virtual_display_path(new_display_path) else {
            return;
        };

        let descendant_prefix = if old_relative.is_empty() {
            None
        } else {
            Some(format!("{old_relative}/"))
        };
        let mut known = self
            .known_remote_paths
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let updates = known
            .iter()
            .filter_map(|((known_backend, relative_path), stable_id)| {
                if known_backend != &backend_id.0 {
                    return None;
                }
                let matches_exact = relative_path == &old_relative;
                let matches_descendant = descendant_prefix
                    .as_ref()
                    .is_some_and(|prefix| relative_path.starts_with(prefix));
                if matches_exact || matches_descendant {
                    Some((relative_path.clone(), stable_id.clone()))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        for (old_relative_path, stable_id) in updates {
            known.remove(&(backend_id.0.clone(), old_relative_path.clone()));
            let suffix = old_relative_path
                .strip_prefix(&old_relative)
                .unwrap_or_default();
            known.insert(
                (backend_id.0.clone(), format!("{new_relative}{suffix}")),
                stable_id,
            );
        }
    }

    pub(super) fn forget_known_remote_path_prefix(
        &self,
        backend_id: &BackendId,
        display_path: &str,
    ) {
        let Some((_, relative_prefix)) = self.parse_virtual_display_path(display_path) else {
            return;
        };
        let descendant_prefix = if relative_prefix.is_empty() {
            None
        } else {
            Some(format!("{relative_prefix}/"))
        };
        let mut known = self
            .known_remote_paths
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let stale_keys = known
            .keys()
            .filter(|(known_backend, relative_path)| {
                known_backend == &backend_id.0
                    && (relative_path == &relative_prefix
                        || descendant_prefix
                            .as_ref()
                            .is_some_and(|prefix| relative_path.starts_with(prefix)))
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in stale_keys {
            known.remove(&key);
        }
    }

    pub(super) async fn reconcile_cache_after_move(
        &self,
        source: &ObjectRef,
        src_resolved: &ResolvedBackend,
        destination: &Path,
        dst_resolved: &ResolvedBackend,
    ) {
        if src_resolved.backend_id != dst_resolved.backend_id {
            self.purge_cached_display_path_prefix(&src_resolved.backend_id, &source.display_path);
            return;
        }

        let destination_display_path = destination.to_string_lossy().to_string();
        let destination_object = match self.object_ref_for_path(destination, dst_resolved) {
            Ok(object) => object,
            Err(_) => {
                self.purge_cached_display_path_prefix(
                    &src_resolved.backend_id,
                    &source.display_path,
                );
                return;
            }
        };

        let destination_stable_id = if self.parse_virtual_path(destination).is_some() {
            dst_resolved
                .adapter
                .get_stable_id(&destination_object)
                .await
                .ok()
        } else {
            Some(destination_object.key.stable_id.clone())
        };

        match destination_stable_id {
            Some(stable_id) if stable_id == source.key.stable_id => {
                self.rebind_cached_display_path_prefix(
                    &src_resolved.backend_id,
                    &source.display_path,
                    &destination_display_path,
                );
                if let Some((backend_id, relative_path)) =
                    self.parse_virtual_display_path(&destination_display_path)
                {
                    self.remember_remote_path(&backend_id, &relative_path, &stable_id);
                }
            }
            Some(stable_id) => {
                self.purge_cached_display_path_prefix(
                    &src_resolved.backend_id,
                    &source.display_path,
                );
                if let Some((backend_id, relative_path)) =
                    self.parse_virtual_display_path(&destination_display_path)
                {
                    self.remember_remote_path(&backend_id, &relative_path, &stable_id);
                }
            }
            None => {
                self.purge_cached_display_path_prefix(
                    &src_resolved.backend_id,
                    &source.display_path,
                );
            }
        }
    }
}

fn stat_local_path_sync(path: &Path) -> Result<MetadataResult, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    Ok(MetadataResult {
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn local_breadcrumbs_start_at_the_root() {
        assert_eq!(
            local_breadcrumbs(Path::new("/Users/me")),
            vec![
                ("/".to_string(), "/".to_string()),
                ("Users".to_string(), "/Users".to_string()),
                ("me".to_string(), "/Users/me".to_string()),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn local_breadcrumbs_start_at_the_drive() {
        assert_eq!(
            local_breadcrumbs(Path::new(r"C:\Users")),
            vec![
                ("C:".to_string(), r"C:\".to_string()),
                ("Users".to_string(), r"C:\Users".to_string()),
            ]
        );
    }

    #[test]
    fn stat_local_path_sync_reports_file_and_directory_kind() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("file.txt");
        let dir_path = temp.path().join("folder");
        std::fs::write(&file_path, b"contents").expect("write file");
        std::fs::create_dir(&dir_path).expect("create dir");

        let file_metadata = stat_local_path_sync(&file_path).expect("file metadata");
        assert!(!file_metadata.is_dir);
        assert_eq!(file_metadata.size, Some(8));
        assert_eq!(file_metadata.level, MetadataLevel::L1);

        let dir_metadata = stat_local_path_sync(&dir_path).expect("dir metadata");
        assert!(dir_metadata.is_dir);
        assert_eq!(dir_metadata.size, None);
        assert_eq!(dir_metadata.level, MetadataLevel::L1);
    }

    #[test]
    fn record_directory_snapshot_persists_complete_item_count() {
        let runtime = AppRuntime::initialize();
        let backend_id = BackendId("snapshot-test".to_string());
        let stable_id = StableId(format!(
            "dir-{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        ));

        runtime.record_directory_snapshot(&backend_id, &stable_id, 7, true);

        let snapshot = runtime
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_snapshot(&backend_id.0, &stable_id.0)
            .expect("load snapshot")
            .expect("snapshot exists");
        assert_eq!(snapshot.item_count, 7);
        assert!(snapshot.is_complete);
    }

    #[test]
    fn cached_directory_entries_restore_completed_generation_and_filter_hidden_items() {
        let runtime = AppRuntime::initialize();
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_string_lossy().to_string();
        let backend_id = runtime.backend_id_for_path(temp.path());
        let parent_stable_id = runtime.stable_id_for_path(temp.path());
        let entry = |name: &str, stable_id: &str| iris_core_domain::types::list::ListEntry {
            object_ref: ObjectRef {
                key: ObjectKey {
                    backend_id: backend_id.clone(),
                    stable_id: StableId(stable_id.to_string()),
                },
                parent_stable_id: Some(parent_stable_id.clone()),
                display_path: temp.path().join(name).to_string_lossy().to_string(),
            },
            sort_name_key: iris_core_domain::types::list::ListEntry::sort_name_key_for(name),
            sort_extension_key: iris_core_domain::types::list::ListEntry::sort_extension_key_for(
                name,
            ),
            name: name.to_string(),
            is_dir: false,
            metadata_level: MetadataLevel::L1,
            size: Some(12),
            mtime: None,
            ctime: None,
            is_symlink: false,
            symlink_target: None,
            is_inaccessible: false,
            annotation: None,
        };
        let entries = vec![
            entry("visible.txt", "visible-id"),
            entry(".hidden", "hidden-id"),
        ];
        let write = runtime
            .begin_directory_snapshot_for_path(&root)
            .expect("snapshot generation");
        assert!(runtime.append_directory_snapshot_entries(&write, &entries));
        assert!(runtime.complete_directory_snapshot(write));
        assert!(runtime.has_recent_complete_directory_snapshot(&root));

        let visible = runtime.cached_directory_entries(&root, false);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "visible.txt");
        assert_eq!(visible[0].size, Some(12));

        let all = runtime.cached_directory_entries(&root, true);
        assert_eq!(all.len(), 2);

        let mut snapshot = runtime
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_snapshot(&backend_id.0, &parent_stable_id.0)
            .expect("load snapshot")
            .expect("snapshot exists");
        snapshot.snapshot_at =
            SystemTime::now() - DIRECTORY_SNAPSHOT_FAST_PATH_TTL - Duration::from_secs(1);
        runtime
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .upsert_snapshot(&snapshot)
            .expect("age snapshot");
        assert!(!runtime.has_recent_complete_directory_snapshot(&root));
    }

    #[test]
    fn path_completion_candidates_include_catalog_prefix_matches() {
        let runtime = AppRuntime::initialize();
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let root = format!("/tmp/iris-path-completion-{unique}");
        let child = format!("{root}/child");
        runtime
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .upsert_object(&CachedObject {
                backend_id: "local".to_string(),
                stable_id: format!("path-completion-{unique}"),
                display_path: child.clone(),
                size: None,
                mtime: None,
                content_type: None,
                etag: None,
                cached_level: MetadataLevel::L1,
                last_accessed: SystemTime::now(),
            })
            .expect("catalog object insert");

        let candidates = runtime.path_completion_candidates(&format!("{root}/ch"), &root, 8);

        assert!(candidates.contains(&child));
    }
}
