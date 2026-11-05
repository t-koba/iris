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
        let resolved_transport = match primary_mapping.transport {
            RemoteTransportKind::Disabled => return Ok(None),
            RemoteTransportKind::Auto => {
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
        Ok(Some(sum_transferred_bytes(items).await))
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
}
