use super::*;

struct RsyncBatchRequest<'a> {
    strategy: RsyncStrategy,
    items: &'a [PendingFileItem],
    dest_dir: &'a Path,
    is_move: bool,
    resolution: &'a ConflictResolution,
    src_mapping: Option<&'a crate::remote_transport::RemoteTransportMapping>,
    dst_mapping: Option<&'a crate::remote_transport::RemoteTransportMapping>,
}

struct SftpBatchPlan {
    transfer: crate::remote_transport::SftpTransferPlan,
}

fn batch_source_is_directory(item: &PendingFileItem) -> bool {
    Path::new(&item.source.display_path).is_dir()
}

/// rsync places every source under its own basename in the destination directory and
/// never receives the per-item requested basename. A batch whose requested name differs
/// would be recorded as installed at a path rsync did not write, which Undo and Redo
/// later treat as their own. Native SFTP receives an explicit destination per item, so
/// this restriction belongs to rsync only and must not suppress a replay batch that
/// SFTP could execute safely.
fn rsync_batch_preserves_names(items: &[PendingFileItem]) -> bool {
    items.iter().all(|item| {
        let source_name = Path::new(&item.source.display_path).file_name();
        source_name.is_some() && source_name == Path::new(&item.destination_path).file_name()
    })
}

impl AppRuntime {
    pub async fn try_rsync_batch(
        &self,
        items: &[PendingFileItem],
        is_move: bool,
        resolution: &ConflictResolution,
    ) -> Result<Option<u64>, String> {
        use crate::remote_transport::RemoteTransportKind;

        if items.is_empty() {
            return Ok(None);
        }

        // An aggregate batch result cannot prove that a directory was newly created:
        // rsync/SFTP happily merge a source folder into an existing destination folder.
        // Fresh batches and Undo/Redo replay alike must leave directories to the per-item
        // path, which records creation as the condition for journaling them.
        if items.iter().any(batch_source_is_directory) {
            return Ok(None);
        }

        for item in items {
            if self.is_virtual_display_path(&item.source.display_path)
                || self.is_virtual_display_path(&item.destination_path)
            {
                return Ok(None);
            }
            if !Path::new(&item.source.display_path).is_absolute()
                || !Path::new(&item.destination_path).is_absolute()
            {
                return Ok(None);
            }
        }

        let mut dest_dirs = std::collections::HashSet::new();
        for item in items {
            if let Some(parent) = Path::new(&item.destination_path).parent() {
                dest_dirs.insert(parent.to_path_buf());
            }
        }
        if dest_dirs.len() != 1 {
            return Ok(None);
        }
        let Some(dest_dir) = dest_dirs.into_iter().next() else {
            return Ok(None);
        };

        let src_path0 = Path::new(&items[0].source.display_path);
        let (dst_mapping, src_mapping) = self.lookup_transport_mappings(&dest_dir, src_path0);
        if dst_mapping.is_none() && src_mapping.is_none() {
            return Ok(None);
        }

        let Some(primary_mapping) = dst_mapping.as_ref().or(src_mapping.as_ref()) else {
            return Ok(None);
        };
        let mut validated = false;
        let resolved_transport = match primary_mapping.transport {
            RemoteTransportKind::Disabled => return Ok(None),
            RemoteTransportKind::Auto => {
                self.validate_batch(items).await?;
                validated = true;
                if let Some(bytes) = self
                    .run_sftp_batch(
                        items,
                        is_move,
                        resolution,
                        src_mapping.as_ref(),
                        dst_mapping.as_ref(),
                    )
                    .await?
                {
                    return Ok(Some(bytes));
                }
                RemoteTransportKind::RsyncSsh
            }
            RemoteTransportKind::Sftp => {
                self.validate_batch(items).await?;
                return self
                    .run_sftp_batch(
                        items,
                        is_move,
                        resolution,
                        src_mapping.as_ref(),
                        dst_mapping.as_ref(),
                    )
                    .await;
            }
            other => other,
        };

        let strategy = match resolved_transport {
            RemoteTransportKind::RsyncSsh => RsyncStrategy::Ssh,
            _ => return Ok(None),
        };
        // Native SFTP uses the explicit destination in its plan. Only rsync loses the
        // requested per-item name, so decline a wrong-sibling batch before validation
        // and before any destination mutation.
        if !rsync_batch_preserves_names(items) {
            return Ok(None);
        }
        if !validated {
            self.validate_batch(items).await?;
        }

        self.run_rsync_batch(RsyncBatchRequest {
            strategy,
            items,
            dest_dir: &dest_dir,
            is_move,
            resolution,
            src_mapping: src_mapping.as_ref(),
            dst_mapping: dst_mapping.as_ref(),
        })
        .await
    }

    /// Validate the entire batch before any external tool can mutate a destination.
    async fn validate_batch(&self, items: &[PendingFileItem]) -> Result<(), String> {
        for item in items {
            self.ensure_safe_transfer_destination(&item.source, Path::new(&item.destination_path))
                .await?;
        }
        Ok(())
    }

    fn lookup_transport_mappings(
        &self,
        dest_dir: &Path,
        src_path: &Path,
    ) -> (
        Option<crate::remote_transport::RemoteTransportMapping>,
        Option<crate::remote_transport::RemoteTransportMapping>,
    ) {
        use crate::remote_transport::MappingTable;
        let guard = self
            .remote_transport_mappings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        (
            MappingTable::lookup_for_local_path(&guard, dest_dir).cloned(),
            MappingTable::lookup_for_local_path(&guard, src_path).cloned(),
        )
    }

    async fn run_rsync_batch(&self, request: RsyncBatchRequest<'_>) -> Result<Option<u64>, String> {
        let RsyncBatchRequest {
            strategy,
            items,
            dest_dir,
            is_move,
            resolution,
            src_mapping,
            dst_mapping,
        } = request;
        if !rsync_binary_available().await {
            return Ok(None);
        }

        // A Move batch passes --remove-source-files, so source sizes must be captured
        // before rsync runs; the aggregate completion reports these bytes to job progress.
        let transferred_bytes = sum_transferred_bytes(items).await;

        let mut cmd = tokio::process::Command::new("rsync");
        cmd.arg("-a").arg("--protect-args").arg("--info=progress2");

        if matches!(strategy, RsyncStrategy::Ssh) {
            let shell_opt = dst_mapping
                .and_then(|m| m.rsync_ssh_remote_shell())
                .or_else(|| src_mapping.and_then(|m| m.rsync_ssh_remote_shell()));
            if let Some(shell) = shell_opt {
                cmd.arg("-e").arg(shell);
            }
        }

        if matches!(resolution, ConflictResolution::Skip) {
            cmd.arg("--ignore-existing");
        }
        if is_move {
            cmd.arg("--remove-source-files");
        }

        for item in items {
            let src_path = Path::new(&item.source.display_path);
            if let Some(map) = src_mapping {
                let Some(spec) = strategy.remote_spec(map, src_path) else {
                    return Ok(None);
                };
                cmd.arg(spec);
            } else {
                cmd.arg(&item.source.display_path);
            }
        }

        if let Some(map) = dst_mapping {
            let Some(spec) = strategy.remote_spec(map, dest_dir) else {
                return Ok(None);
            };
            cmd.arg(format!("{}/", spec));
        } else {
            cmd.arg(format!("{}/", dest_dir.display()));
        }

        let label = strategy.label();
        tracing::info!("rsync ({}) command: {:?}", label, cmd);
        let output = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| format!("rsync {} launch failed: {}", label, e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("rsync ({}) failed, falling back: {}", label, stderr);
            return Ok(None);
        }
        Ok(Some(transferred_bytes))
    }

    async fn run_sftp_batch(
        &self,
        items: &[PendingFileItem],
        is_move: bool,
        resolution: &ConflictResolution,
        src_mapping: Option<&crate::remote_transport::RemoteTransportMapping>,
        dst_mapping: Option<&crate::remote_transport::RemoteTransportMapping>,
    ) -> Result<Option<u64>, String> {
        let Some(plan) =
            build_sftp_batch_plan(items, is_move, resolution, src_mapping, dst_mapping)
        else {
            return Ok(None);
        };

        tracing::info!(
            direction = ?plan.transfer.direction,
            entries = plan.transfer.entries.len(),
            host = %plan.transfer.mapping.host,
            "executing native SFTP batch"
        );
        let transferred_bytes = sum_transferred_bytes(items).await;
        let started = std::time::Instant::now();
        if let Err(error) = crate::remote_transport::execute_sftp_plan(&plan.transfer).await {
            iris_transfer::method_selector::observe_backend_sample("sftp", started.elapsed(), 0);
            tracing::warn!("native sftp batch failed, falling back: {error}");
            return Ok(None);
        }

        iris_transfer::method_selector::observe_backend_sample(
            "sftp",
            started.elapsed(),
            transferred_bytes,
        );
        Ok(Some(transferred_bytes))
    }
}

fn build_sftp_batch_plan(
    items: &[PendingFileItem],
    is_move: bool,
    resolution: &ConflictResolution,
    src_mapping: Option<&crate::remote_transport::RemoteTransportMapping>,
    dst_mapping: Option<&crate::remote_transport::RemoteTransportMapping>,
) -> Option<SftpBatchPlan> {
    if !matches!(resolution, ConflictResolution::Overwrite) {
        return None;
    }

    let (direction, mapping) = match (src_mapping, dst_mapping) {
        (Some(src), None) => (
            crate::remote_transport::SftpTransferDirection::Download,
            src,
        ),
        (None, Some(dst)) => (crate::remote_transport::SftpTransferDirection::Upload, dst),
        _ => return None,
    };

    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let local_source = Path::new(&item.source.display_path);
        let local_destination = Path::new(&item.destination_path);
        let metadata = std::fs::metadata(local_source).ok()?;
        let is_dir = metadata.is_dir();

        let entry = match direction {
            crate::remote_transport::SftpTransferDirection::Upload => {
                let remote_destination = mapping.remote_path_for(local_destination)?;
                crate::remote_transport::SftpTransferEntry {
                    local_path: local_source.to_path_buf(),
                    remote_path: remote_destination,
                    is_dir,
                }
            }
            crate::remote_transport::SftpTransferDirection::Download => {
                let remote_source = mapping.remote_path_for(local_source)?;
                crate::remote_transport::SftpTransferEntry {
                    local_path: local_destination.to_path_buf(),
                    remote_path: remote_source,
                    is_dir,
                }
            }
        };
        entries.push(entry);
    }

    Some(SftpBatchPlan {
        transfer: crate::remote_transport::SftpTransferPlan {
            mapping: mapping.clone(),
            direction,
            delete_sources: is_move,
            pipeline_depth: iris_transfer::method_selector::recommended_workers_for_backend(
                "sftp",
                crate::remote_transport::DEFAULT_SFTP_PIPELINE_DEPTH,
            )
            .clamp(
                crate::remote_transport::DEFAULT_SFTP_PIPELINE_DEPTH,
                crate::remote_transport::MAX_SFTP_PIPELINE_DEPTH,
            ),
            entries,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core_domain::types::object::{BackendId, ObjectKey, StableId};

    fn object_ref(display_path: &Path) -> ObjectRef {
        ObjectRef {
            local_path_is_lossy: false,
            key: ObjectKey {
                backend_id: BackendId("local".into()),
                stable_id: StableId(display_path.to_string_lossy().to_string()),
            },
            parent_stable_id: None,
            display_path: display_path.to_string_lossy().to_string(),
        }
    }

    fn mapping(root: &Path) -> crate::remote_transport::RemoteTransportMapping {
        crate::remote_transport::RemoteTransportMapping {
            id: "m1".into(),
            local_mount_path: root.to_string_lossy().to_string(),
            host: "nas.local".into(),
            ssh_port: 2222,
            ssh_username: "admin".into(),
            remote_base_path: "/volume1/data".into(),
            transport: crate::remote_transport::RemoteTransportKind::Sftp,
            auth: crate::remote_transport::SshAuthKind::Agent,
            ssh_identity_file: String::new(),
            ssh_password: String::new(),
            enabled: true,
            auto_detected: false,
            notes: String::new(),
        }
    }

    #[test]
    fn builds_sftp_upload_plan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mount_root = temp.path().join("mount");
        let source_root = temp.path().join("local");
        std::fs::create_dir_all(&mount_root).expect("mount root");
        std::fs::create_dir_all(&source_root).expect("source root");

        let source = source_root.join("photo.jpg");
        std::fs::write(&source, b"jpeg").expect("source file");
        let destination = mount_root.join("album").join("photo.jpg");
        std::fs::create_dir_all(destination.parent().expect("destination parent"))
            .expect("destination parent");

        let items = vec![PendingFileItem {
            source: object_ref(&source),
            destination_path: destination.to_string_lossy().to_string(),
            installed_path: None,
            undoable: true,
        }];
        let map = mapping(&mount_root);

        let plan = build_sftp_batch_plan(
            &items,
            false,
            &ConflictResolution::Overwrite,
            None,
            Some(&map),
        )
        .expect("upload plan");

        assert_eq!(
            plan.transfer.direction,
            crate::remote_transport::SftpTransferDirection::Upload
        );
        assert_eq!(
            plan.transfer.entries,
            vec![crate::remote_transport::SftpTransferEntry {
                local_path: source,
                remote_path: "/volume1/data/album/photo.jpg".into(),
                is_dir: false,
            }]
        );
    }

    #[test]
    fn builds_sftp_download_plan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mount_root = temp.path().join("mount");
        let local_root = temp.path().join("local");
        std::fs::create_dir_all(&mount_root).expect("mount root");
        std::fs::create_dir_all(&local_root).expect("local root");

        let mounted_source = mount_root.join("album").join("photo.jpg");
        std::fs::create_dir_all(mounted_source.parent().expect("source parent"))
            .expect("source parent");
        std::fs::write(&mounted_source, b"jpeg").expect("mounted source");
        let destination = local_root.join("photo.jpg");

        let items = vec![PendingFileItem {
            source: object_ref(&mounted_source),
            destination_path: destination.to_string_lossy().to_string(),
            installed_path: None,
            undoable: true,
        }];
        let map = mapping(&mount_root);

        let plan = build_sftp_batch_plan(
            &items,
            true,
            &ConflictResolution::Overwrite,
            Some(&map),
            None,
        )
        .expect("download plan");

        assert_eq!(
            plan.transfer.direction,
            crate::remote_transport::SftpTransferDirection::Download
        );
        assert_eq!(
            plan.transfer.entries,
            vec![crate::remote_transport::SftpTransferEntry {
                local_path: destination,
                remote_path: "/volume1/data/album/photo.jpg".into(),
                is_dir: false,
            }]
        );
        assert!(plan.transfer.delete_sources);
    }
    #[test]
    fn batch_detects_directory_sources() {
        let temp = tempfile::tempdir().expect("tempdir");
        let directory = temp.path().join("album");
        let file = temp.path().join("photo.jpg");
        std::fs::create_dir(&directory).expect("directory");
        std::fs::write(&file, b"jpeg").expect("file");

        let directory_item = PendingFileItem {
            source: object_ref(&directory),
            destination_path: temp.path().join("out-album").to_string_lossy().into_owned(),
            installed_path: None,
            undoable: true,
        };
        let file_item = PendingFileItem {
            source: object_ref(&file),
            destination_path: temp
                .path()
                .join("out-photo.jpg")
                .to_string_lossy()
                .into_owned(),
            installed_path: None,
            undoable: true,
        };

        assert!(batch_source_is_directory(&directory_item));
        assert!(!batch_source_is_directory(&file_item));
        assert!([directory_item, file_item]
            .iter()
            .any(batch_source_is_directory));
    }
    fn runtime_with_mapping(
        root: &Path,
        transport: crate::remote_transport::RemoteTransportKind,
    ) -> Arc<AppRuntime> {
        let runtime = AppRuntime::initialize();
        let mut map = mapping(root);
        map.transport = transport;
        runtime.set_remote_transport_mappings(vec![map]);
        runtime
    }

    #[test]
    fn sftp_batch_plan_keeps_numbered_replay_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mount_root = temp.path().join("mount");
        let source_root = temp.path().join("local");
        std::fs::create_dir_all(&mount_root).expect("mount root");
        std::fs::create_dir_all(&source_root).expect("source root");
        let source = source_root.join("photo.jpg");
        std::fs::write(&source, b"jpeg").expect("source file");
        let destination = mount_root.join("album").join("photo (1).jpg");
        std::fs::create_dir_all(destination.parent().expect("destination parent"))
            .expect("destination parent");
        let items = [PendingFileItem {
            source: object_ref(&source),
            destination_path: destination.to_string_lossy().into_owned(),
            installed_path: None,
            undoable: true,
        }];

        let plan = build_sftp_batch_plan(
            &items,
            false,
            &ConflictResolution::Overwrite,
            None,
            Some(&mapping(&mount_root)),
        )
        .expect("numbered SFTP batch plan");

        assert_eq!(
            plan.transfer.entries,
            vec![crate::remote_transport::SftpTransferEntry {
                local_path: source,
                remote_path: "/volume1/data/album/photo (1).jpg".into(),
                is_dir: false,
            }]
        );
    }

    #[tokio::test]
    async fn renamed_replay_batch_declines_rsync_but_reaches_sftp_validation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("photo.jpg");
        std::fs::write(&source, b"jpeg").expect("source file");
        let destination_dir = temp.path().join("out");
        std::fs::create_dir(&destination_dir).expect("destination directory");
        let sentinel = destination_dir.join("photo.jpg");
        std::fs::write(&sentinel, b"untouched").expect("sentinel");
        let renamed = destination_dir.join("photo (1).jpg");
        let item = |source_ref| PendingFileItem {
            source: source_ref,
            destination_path: renamed.to_string_lossy().into_owned(),
            installed_path: None,
            undoable: true,
        };

        // rsync would write the sentinel's name rather than the numbered replay name.
        // A lossy source would fail preflight, so returning None proves the rsync-only
        // name guard ran before validation and before any destination mutation.
        let mut rsync_source_ref = object_ref(&source);
        rsync_source_ref.local_path_is_lossy = true;
        let rsync = runtime_with_mapping(
            &destination_dir,
            crate::remote_transport::RemoteTransportKind::RsyncSsh,
        );
        let declined = rsync
            .try_rsync_batch(
                &[item(rsync_source_ref)],
                false,
                &ConflictResolution::Overwrite,
            )
            .await
            .expect("renamed batch declined for rsync");
        assert!(declined.is_none());
        assert_eq!(
            std::fs::read(&sentinel).expect("sentinel intact"),
            b"untouched"
        );
        assert!(!renamed.exists());

        // Native SFTP receives explicit per-item destinations, so a renamed replay must
        // no longer be declined before validation. A lossy source makes the reached
        // preflight deterministic without contacting a remote host.
        let mut source_ref = object_ref(&source);
        source_ref.local_path_is_lossy = true;
        let sftp = runtime_with_mapping(
            &destination_dir,
            crate::remote_transport::RemoteTransportKind::Sftp,
        );
        let error = sftp
            .try_rsync_batch(
                &[item(source_ref.clone())],
                false,
                &ConflictResolution::Overwrite,
            )
            .await
            .expect_err("renamed batch reaches SFTP preflight");
        assert!(error.contains("losslessly"), "{error}");
        assert!(!renamed.exists());

        // Auto tries native SFTP first. Its rewritten arm must also validate before
        // handing anything to a backend instead of declining the numbered replay early.
        let auto = runtime_with_mapping(
            &destination_dir,
            crate::remote_transport::RemoteTransportKind::Auto,
        );
        let error = auto
            .try_rsync_batch(&[item(source_ref)], false, &ConflictResolution::Overwrite)
            .await
            .expect_err("renamed batch reaches Auto preflight");
        assert!(error.contains("losslessly"), "{error}");
        assert!(!renamed.exists());
    }

    #[tokio::test]
    async fn transferred_bytes_are_captured_before_move_removes_sources() {
        let temp = tempfile::tempdir().expect("tempdir");
        let items: Vec<_> = [("first.bin", 17usize), ("second.bin", 34)]
            .into_iter()
            .map(|(name, size)| {
                let source = temp.path().join(name);
                std::fs::write(&source, vec![1u8; size]).expect("source file");
                PendingFileItem {
                    source: object_ref(&source),
                    destination_path: temp
                        .path()
                        .join("out")
                        .join(name)
                        .to_string_lossy()
                        .into_owned(),
                    installed_path: None,
                    undoable: true,
                }
            })
            .collect();

        // Mirrors run_rsync_batch: snapshot the sizes first, then let a Move remove them.
        let transferred_bytes = sum_transferred_bytes(&items).await;
        for item in &items {
            std::fs::remove_file(Path::new(&item.source.display_path)).expect("source removed");
        }
        assert_eq!(transferred_bytes, 51);
        assert_eq!(sum_transferred_bytes(&items).await, 0);
    }

    #[tokio::test]
    async fn directory_source_batch_declines_before_transfer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let directory = temp.path().join("album");
        std::fs::create_dir(&directory).expect("directory");
        // This is a fresh batch: no Undo/Redo replay is involved. A destination inside
        // the source would be rejected by validation, so reaching that error would prove
        // the batch was not declined up front. The directory must instead go to the
        // per-item path that records creation ownership.
        let items = vec![PendingFileItem {
            source: object_ref(&directory),
            destination_path: directory.join("nested").to_string_lossy().into_owned(),
            installed_path: None,
            undoable: true,
        }];

        let declined = app_runtime()
            .try_rsync_batch(&items, false, &ConflictResolution::Overwrite)
            .await
            .expect("fresh directory batch declined before validation");
        assert!(declined.is_none());
        assert!(!directory.join("nested").exists());
    }
}
