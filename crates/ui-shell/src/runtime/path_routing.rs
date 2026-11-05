use super::*;

impl AppRuntime {
    pub(super) fn register_volume_backend(&self, volume: &MountedVolume) -> Option<BackendId> {
        let backend_id = match volume.kind {
            VolumeKind::Nfs => BackendId(format!("nfs:{}", volume.mount_path.display())),
            _ => return None,
        };

        let mut registered_backends = self
            .registered_backends
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if registered_backends
            .iter()
            .any(|backend| backend.root_path == volume.mount_path)
        {
            return Some(backend_id);
        }

        let adapter: Arc<dyn BackendAdapter> = match volume.kind {
            VolumeKind::Nfs => Arc::new(LocalFsAdapter::nfs_mount(
                volume.mount_path.clone(),
                backend_id.clone(),
            )),
            _ => return None,
        };

        self.registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .register(backend_id.clone(), adapter);
        registered_backends.push(RegisteredBackend {
            backend_id: backend_id.clone(),
            root_path: volume.mount_path.clone(),
            kind: volume.kind,
            display_name: volume.name.clone(),
            root_stable_id: get_local_stable_id(&volume.mount_path)
                .unwrap_or_else(|_| StableId(volume.mount_path.to_string_lossy().to_string())),
        });
        Some(backend_id)
    }

    pub(super) fn resolve_backend(&self, path: &Path) -> ResolvedBackend {
        let registered_backends = self
            .registered_backends
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();

        let mut selected = registered_backends
            .into_iter()
            .filter(|backend| path.starts_with(&backend.root_path))
            .max_by_key(|backend| backend.root_path.as_os_str().len());

        if let Some(backend) = selected.take() {
            let adapter = self
                .registry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&backend.backend_id)
                .unwrap_or_else(|| panic!("missing adapter for {}", backend.backend_id.0));
            return ResolvedBackend {
                backend_id: backend.backend_id,
                root_path: backend.root_path,
                kind: backend.kind,
                root_stable_id: backend.root_stable_id,
                adapter,
            };
        }

        #[cfg(windows)]
        let (backend_id, root_path) = local_backend_for_path(path);
        #[cfg(not(windows))]
        let (backend_id, root_path) = (BackendId("local".to_string()), default_local_root());

        let adapter = self.ensure_local_adapter(&backend_id, &root_path);
        ResolvedBackend {
            backend_id,
            root_path,
            kind: VolumeKind::Local,
            root_stable_id: StableId("root".to_string()),
            adapter,
        }
    }

    pub(super) fn object_ref_for_path(
        &self,
        path: &Path,
        resolved: &ResolvedBackend,
    ) -> Result<ObjectRef, String> {
        if self.parse_virtual_path(path).is_some() {
            let relative_path = self.relative_display_path(path, resolved)?;
            let stable_id = if relative_path.is_empty() {
                resolved.root_stable_id.clone()
            } else {
                self.known_remote_paths
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .get(&(resolved.backend_id.0.clone(), relative_path.clone()))
                    .cloned()
                    .unwrap_or_else(|| {
                        StableId(format!(
                            "pending:{}:{}",
                            resolved.backend_id.0, relative_path
                        ))
                    })
            };
            let parent_stable_id =
                parent_relative_display_path(&relative_path).and_then(|parent| {
                    if parent.is_empty() {
                        Some(resolved.root_stable_id.clone())
                    } else {
                        self.known_remote_paths
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .get(&(resolved.backend_id.0.clone(), parent))
                            .cloned()
                    }
                });
            return Ok(ObjectRef {
                key: ObjectKey {
                    backend_id: resolved.backend_id.clone(),
                    stable_id,
                },
                parent_stable_id,
                display_path: relative_path,
            });
        }

        let relative = path
            .strip_prefix(&resolved.root_path)
            .map_err(|error| error.to_string())?;
        let stable_id = std::fs::metadata(path)
            .ok()
            .and_then(|_| get_local_stable_id(path).ok())
            .unwrap_or_else(|| StableId(format!("pending:{}", path.to_string_lossy())));
        Ok(ObjectRef {
            key: ObjectKey {
                backend_id: resolved.backend_id.clone(),
                stable_id,
            },
            parent_stable_id: None,
            display_path: relative.to_string_lossy().to_string(),
        })
    }

    pub(super) fn same_backend(
        &self,
        src: &Path,
        src_resolved: &ResolvedBackend,
        dst: &Path,
        dst_resolved: &ResolvedBackend,
    ) -> bool {
        if src_resolved.backend_id != dst_resolved.backend_id {
            return false;
        }

        if src_resolved.kind != VolumeKind::Local {
            return true;
        }

        same_filesystem(src, dst)
    }

    pub fn dnd_operation_for_drop(
        &self,
        sources: &[ObjectRef],
        destination_dir: &Path,
        modifiers: iced::keyboard::Modifiers,
    ) -> DndOperation {
        self.dnd_operation_for_paths(
            &sources
                .iter()
                .map(|source| PathBuf::from(&source.display_path))
                .collect::<Vec<_>>(),
            destination_dir,
            modifiers,
        )
    }

    pub fn dnd_operation_for_paths(
        &self,
        source_paths: &[PathBuf],
        destination_dir: &Path,
        modifiers: iced::keyboard::Modifiers,
    ) -> DndOperation {
        if let Some(forced) = forced_dnd_operation(modifiers) {
            return forced;
        }

        let should_move = !source_paths.is_empty()
            && source_paths
                .iter()
                .all(|source| self.should_move_path_by_default_for_drop(source, destination_dir));

        if should_move {
            DndOperation::Move
        } else {
            DndOperation::Copy
        }
    }

    pub(super) fn should_move_path_by_default_for_drop(
        &self,
        source: &Path,
        destination_dir: &Path,
    ) -> bool {
        let src_resolved = self.resolve_backend(source);
        let dst_resolved = self.resolve_backend(destination_dir);

        self.same_backend(source, &src_resolved, destination_dir, &dst_resolved)
    }

    pub fn object_ref_for_drop_path(&self, path: &Path) -> Result<ObjectRef, String> {
        let resolved = self.resolve_backend(path);
        self.object_ref_for_path(path, &resolved)
    }

    pub(super) fn source_object_ref(
        &self,
        source: &ObjectRef,
        resolved: &ResolvedBackend,
    ) -> Result<ObjectRef, String> {
        let path = Path::new(&source.display_path);
        let mut object = self.object_ref_for_path(path, resolved)?;
        object.key.stable_id = source.key.stable_id.clone();
        object.parent_stable_id = source.parent_stable_id.clone();
        Ok(object)
    }

    pub(super) fn ensure_local_adapter(
        &self,
        backend_id: &BackendId,
        root_path: &Path,
    ) -> Arc<dyn BackendAdapter> {
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(adapter) = registry.get(backend_id) {
            return adapter;
        }

        let adapter = Arc::new(
            LocalFsAdapter::new(root_path.to_path_buf(), backend_id.clone())
                .unwrap_or_else(|error| panic!("failed to create local backend: {}", error)),
        );
        registry.register(backend_id.clone(), adapter.clone());
        adapter
    }

    pub(super) fn absolute_display_path(
        &self,
        backend_id: &BackendId,
        relative_path: &str,
    ) -> String {
        let root = virtual_root_path(backend_id).to_string_lossy().to_string();
        if relative_path.is_empty() {
            root
        } else {
            join_display_path(&root, relative_path)
        }
    }

    pub(super) fn parse_virtual_display_path(&self, path: &str) -> Option<(BackendId, String)> {
        self.parse_virtual_path(Path::new(path))
    }

    pub(super) fn resolve_display_named_virtual_path(&self, input: &str) -> Option<String> {
        let registered_backends = self
            .registered_backends
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        let matched = registered_backends
            .iter()
            .filter_map(|backend| {
                let prefix = backend.display_name.as_str();
                if input == prefix {
                    Some((backend.backend_id.clone(), prefix.len()))
                } else {
                    input
                        .strip_prefix(prefix)
                        .filter(|rest| rest.starts_with('/'))
                        .map(|_| (backend.backend_id.clone(), prefix.len()))
                }
            })
            .max_by_key(|(_, prefix_len)| *prefix_len)?;

        let relative = input[matched.1..].trim_start_matches('/');
        Some(self.absolute_display_path(&matched.0, &normalize_relative_display_path(relative)))
    }

    pub(super) fn parse_virtual_path(&self, path: &Path) -> Option<(BackendId, String)> {
        let mut components = path.components();
        let root = components.next()?;
        if root != std::path::Component::RootDir {
            return None;
        }
        let marker = components.next()?;
        if marker.as_os_str() != REMOTE_ROOT_MARKER {
            return None;
        }
        let backend_component = components.next()?;
        let backend_id = BackendId(backend_component.as_os_str().to_string_lossy().to_string());
        let relative = components
            .filter_map(|component| match component {
                std::path::Component::Normal(segment) => {
                    Some(segment.to_string_lossy().to_string())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/");
        Some((backend_id, relative))
    }

    pub(super) fn relative_display_path(
        &self,
        path: &Path,
        resolved: &ResolvedBackend,
    ) -> Result<String, String> {
        if self.parse_virtual_path(path).is_some() {
            return self
                .parse_virtual_path(path)
                .map(|(_, relative)| relative)
                .ok_or_else(|| format!("invalid virtual path: {}", path.display()));
        }
        path.strip_prefix(&resolved.root_path)
            .map_err(|error| error.to_string())
            .map(|relative| relative.to_string_lossy().to_string())
    }

    pub(super) fn object_ref_for_display_path(
        &self,
        path: &Path,
        allow_root: bool,
    ) -> Result<ObjectRef, String> {
        let resolved = self.resolve_backend(path);
        let object = self.object_ref_for_path(path, &resolved)?;
        if !allow_root && object.display_path.is_empty() {
            return Err(format!(
                "{} cannot target backend root directly",
                path.display()
            ));
        }
        Ok(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_pcloud_callback_fragment_extracts_token_uid_and_host() {
        let parsed = parse_pcloud_callback(
            "http://127.0.0.1/iris-pcloud#access_token=token123&uid=42&hostname=eapi.pcloud.com",
        )
        .expect("callback should parse");
        assert_eq!(parsed.access_token, "token123");
        assert_eq!(parsed.account_id, "42");
        assert_eq!(parsed.api_base, "https://eapi.pcloud.com");
    }

    #[test]
    fn join_display_path_preserves_virtual_root_shape() {
        assert_eq!(
            join_display_path("/__iris_remote__/pcloud:42", "photos"),
            "/__iris_remote__/pcloud:42/photos"
        );
        assert_eq!(join_display_path("/", "tmp"), "/tmp");
    }

    #[test]
    fn parent_relative_display_path_handles_nested_paths() {
        assert_eq!(
            parent_relative_display_path("photos/2024/raw"),
            Some("photos/2024".to_string())
        );
        assert_eq!(parent_relative_display_path("photos"), Some(String::new()));
        assert_eq!(parent_relative_display_path(""), None);
    }

    #[test]
    fn dnd_operation_defaults_to_move_on_same_filesystem() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_path = temp.path().join("src.txt");
        let dst_dir = temp.path().join("dst");
        fs::write(&src_path, b"drag-source").expect("write source");
        fs::create_dir(&dst_dir).expect("create dest");

        let source = ObjectRef {
            key: ObjectKey {
                backend_id: BackendId("local".to_string()),
                stable_id: StableId("src".to_string()),
            },
            parent_stable_id: Some(StableId("root".to_string())),
            display_path: src_path.to_string_lossy().to_string(),
        };

        let operation = app_runtime().dnd_operation_for_drop(
            &[source],
            &dst_dir,
            iced::keyboard::Modifiers::default(),
        );

        assert_eq!(operation, DndOperation::Move);
    }

    #[test]
    fn dnd_operation_honors_copy_override_modifier() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_path = temp.path().join("src.txt");
        let dst_dir = temp.path().join("dst");
        fs::write(&src_path, b"drag-source").expect("write source");
        fs::create_dir(&dst_dir).expect("create dest");

        let source = ObjectRef {
            key: ObjectKey {
                backend_id: BackendId("local".to_string()),
                stable_id: StableId("src".to_string()),
            },
            parent_stable_id: Some(StableId("root".to_string())),
            display_path: src_path.to_string_lossy().to_string(),
        };

        #[cfg(target_os = "macos")]
        let modifiers = iced::keyboard::Modifiers::ALT;
        #[cfg(not(target_os = "macos"))]
        let modifiers = iced::keyboard::Modifiers::CTRL;

        let operation = app_runtime().dnd_operation_for_drop(&[source], &dst_dir, modifiers);

        assert_eq!(operation, DndOperation::Copy);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dnd_operation_honors_alias_modifier_combo_on_macos() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_path = temp.path().join("src.txt");
        let dst_dir = temp.path().join("dst");
        fs::write(&src_path, b"drag-source").expect("write source");
        fs::create_dir(&dst_dir).expect("create dest");

        let operation = app_runtime().dnd_operation_for_paths(
            &[src_path],
            &dst_dir,
            iced::keyboard::Modifiers::ALT | iced::keyboard::Modifiers::LOGO,
        );

        assert_eq!(operation, DndOperation::Link);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn dnd_operation_honors_shortcut_modifier_on_windows() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src_path = temp.path().join("src.txt");
        let dst_dir = temp.path().join("dst");
        fs::write(&src_path, b"drag-source").expect("write source");
        fs::create_dir(&dst_dir).expect("create dest");

        let operation = app_runtime().dnd_operation_for_paths(
            &[src_path],
            &dst_dir,
            iced::keyboard::Modifiers::ALT,
        );

        assert_eq!(operation, DndOperation::Link);
    }
}
