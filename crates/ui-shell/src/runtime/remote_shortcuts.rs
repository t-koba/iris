use super::*;

struct RsyncBatchRequest<'a> {
    strategy: RsyncStrategy,
    items: &'a [PendingFileItem],
    dest_dir: &'a Path,
    is_move: bool,
    resolution: &'a ConflictResolution,
    src_mapping: Option<&'a crate::remote_transport::RemoteTransportMapping>,
    dst_mapping: Option<&'a crate::remote_transport::RemoteTransportMapping>,
    cancellation: BatchCancellation<'a>,
}

struct SftpBatchPlan {
    transfer: crate::remote_transport::SftpTransferPlan,
}

/// The job identity a batch is admitted under, carried to each mutation boundary so a
/// cancellation after admission can still veto the write.
#[derive(Clone, Copy)]
struct BatchCancellation<'a> {
    control: Option<&'a TransferControl>,
    job_id: Option<&'a JobId>,
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

fn canceled(control: Option<&TransferControl>) -> bool {
    control.is_some_and(|control| control.cancel_token().is_cancelled())
}

fn ensure_not_canceled(
    runtime: &AppRuntime,
    cancellation: &BatchCancellation<'_>,
) -> Result<(), String> {
    if canceled(cancellation.control)
        || cancellation
            .job_id
            .is_some_and(|job_id| runtime.job_manager.get_state(job_id) == Some(JobState::Canceled))
    {
        Err("transfer canceled".into())
    } else {
        Ok(())
    }
}

impl AppRuntime {
    pub async fn try_rsync_batch(
        &self,
        items: &[PendingFileItem],
        is_move: bool,
        resolution: &ConflictResolution,
    ) -> Result<Option<u64>, String> {
        self.try_rsync_batch_inner(None, items, is_move, resolution, None)
            .await
    }

    /// Registers the dispatch with Cancel on first poll and keeps the control for the
    /// whole batch, so a Cancel between dispatch and execution cannot write anything.
    pub(crate) async fn try_rsync_batch_admitted(
        self: &Arc<Self>,
        job_id: &JobId,
        dispatch_generation: u64,
        items: &[PendingFileItem],
        is_move: bool,
        resolution: &ConflictResolution,
    ) -> Result<Option<u64>, String> {
        let admission = self.admit_transfer_for(job_id, dispatch_generation)?;
        let control = admission.control();
        let result = self
            .try_rsync_batch_inner(Some(job_id), items, is_move, resolution, Some(control))
            .await;
        drop(admission);
        result
    }

    async fn try_rsync_batch_inner(
        &self,
        job_id: Option<&JobId>,
        items: &[PendingFileItem],
        is_move: bool,
        resolution: &ConflictResolution,
        control: Option<&TransferControl>,
    ) -> Result<Option<u64>, String> {
        use crate::remote_transport::RemoteTransportKind;

        let cancellation = BatchCancellation { control, job_id };

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
        let resolved_transport = match primary_mapping.transport {
            RemoteTransportKind::Disabled => return Ok(None),
            RemoteTransportKind::RsyncSsh => {
                // Native SFTP uses the explicit destination in its plan. Only rsync loses the
                // requested per-item name, so decline a wrong-sibling batch before validation
                // and before any destination mutation.
                if !rsync_batch_preserves_names(items) {
                    return Ok(None);
                }
                self.validate_batch(items).await?;
                ensure_not_canceled(self, &cancellation)?;
                RemoteTransportKind::RsyncSsh
            }
            RemoteTransportKind::Sftp | RemoteTransportKind::Auto => {
                self.validate_batch(items).await?;
                ensure_not_canceled(self, &cancellation)?;
                primary_mapping.transport
            }
        };

        let strategy = match resolved_transport {
            RemoteTransportKind::RsyncSsh => RsyncStrategy::Ssh,
            RemoteTransportKind::Sftp | RemoteTransportKind::Auto => {
                ensure_not_canceled(self, &cancellation)?;
                if let Some(bytes) = self
                    .run_sftp_batch(
                        items,
                        is_move,
                        resolution,
                        src_mapping.as_ref(),
                        dst_mapping.as_ref(),
                        cancellation,
                    )
                    .await?
                {
                    return Ok(Some(bytes));
                }
                // Direct SFTP does not fall back to rsync; only Auto does.
                if resolved_transport == RemoteTransportKind::Sftp {
                    return Ok(None);
                }
                // Native SFTP declined the batch; check before switching transports.
                ensure_not_canceled(self, &cancellation)?;
                // As for a direct rsync batch, never let the fallback write a different
                // basename than the one recorded for the item.
                if !rsync_batch_preserves_names(items) {
                    return Ok(None);
                }
                RsyncStrategy::Ssh
            }
            RemoteTransportKind::Disabled => return Ok(None),
        };

        ensure_not_canceled(self, &cancellation)?;
        self.run_rsync_batch(RsyncBatchRequest {
            strategy,
            items,
            dest_dir: &dest_dir,
            is_move,
            resolution,
            src_mapping: src_mapping.as_ref(),
            dst_mapping: dst_mapping.as_ref(),
            cancellation,
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
            cancellation,
        } = request;
        if !rsync_binary_available().await {
            return Ok(None);
        }

        // A Move batch passes --remove-source-files, so source sizes must be captured
        // before rsync runs; the aggregate completion reports these bytes to job progress.
        let transferred_bytes = sum_transferred_bytes(items).await;
        #[cfg(test)]
        let simulated_effect = tests::await_batch_hook(cancellation.job_id).await;
        // Cancel may have arrived while a test barrier waited: recheck before any write.
        ensure_not_canceled(self, &cancellation)?;
        #[cfg(test)]
        if simulated_effect {
            return tests::simulate_batch_effect(items, is_move).await;
        }

        let mut cmd = tokio::process::Command::new("rsync");
        // If the task is dropped mid-execution, kill the child instead of leaving rsync
        // to finish a write or a source removal after Cancel.
        cmd.kill_on_drop(true);
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
        cancellation: BatchCancellation<'_>,
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
        #[cfg(test)]
        let simulated_effect = tests::await_batch_hook(cancellation.job_id).await;
        // Cancel may have arrived while a test barrier waited: recheck before any write.
        ensure_not_canceled(self, &cancellation)?;
        #[cfg(test)]
        if simulated_effect {
            return tests::simulate_batch_effect(items, is_move).await;
        }
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

    /// Pauses an admitted batch after read-only validation, at the boundary where a
    /// transport would start mutating destinations or sources.
    pub(crate) struct BatchHook {
        pub(crate) entered: Arc<tokio::sync::Notify>,
        pub(crate) release: Arc<tokio::sync::Notify>,
        did_enter: Arc<std::sync::atomic::AtomicBool>,
        simulate_effect: bool,
    }

    static BATCH_HOOKS: OnceLock<Mutex<HashMap<u64, Arc<BatchHook>>>> = OnceLock::new();

    pub(crate) fn install_batch_hook(job_id: &JobId, simulate_effect: bool) -> Arc<BatchHook> {
        let hook = Arc::new(BatchHook {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
            did_enter: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            simulate_effect,
        });
        BATCH_HOOKS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(job_id.0, Arc::clone(&hook));
        hook
    }

    pub(crate) fn remove_batch_hook(job_id: &JobId) {
        BATCH_HOOKS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .remove(&job_id.0);
    }

    pub(crate) async fn await_batch_hook(job_id: Option<&JobId>) -> bool {
        let Some(job_id) = job_id else {
            return false;
        };
        let hook = BATCH_HOOKS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .get(&job_id.0)
            .cloned();
        let Some(hook) = hook else {
            return false;
        };
        hook.did_enter
            .store(true, std::sync::atomic::Ordering::SeqCst);
        hook.entered.notify_one();
        hook.release.notified().await;
        let mut hooks = BATCH_HOOKS.get_or_init(Default::default).lock().unwrap();
        if hooks
            .get(&job_id.0)
            .is_some_and(|installed| Arc::ptr_eq(installed, &hook))
        {
            hooks.remove(&job_id.0);
        }
        drop(hooks);
        hook.simulate_effect
    }

    pub(crate) async fn simulate_batch_effect(
        items: &[PendingFileItem],
        is_move: bool,
    ) -> Result<Option<u64>, String> {
        let total_bytes = sum_transferred_bytes(items).await;
        for item in items {
            let source = Path::new(&item.source.display_path);
            let destination = Path::new(&item.destination_path);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            std::fs::copy(source, destination).map_err(|error| error.to_string())?;
            if is_move {
                std::fs::remove_file(source).map_err(|error| error.to_string())?;
            }
        }
        Ok(Some(total_bytes))
    }

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
    async fn numbered_replay_reaches_native_sftp_batch_dispatch() {
        async fn assert_numbered_replay_dispatch(
            job_id: u64,
            transport: crate::remote_transport::RemoteTransportKind,
        ) {
            let temp = tempfile::tempdir().expect("tempdir");
            let source = temp.path().join("photo.jpg");
            std::fs::write(&source, b"jpeg").expect("source file");
            let destination_dir = temp.path().join("out");
            std::fs::create_dir(&destination_dir).expect("destination directory");
            // A sibling named like the source stands in for the already installed file.
            // A fallback to rsync would overwrite it instead of the numbered name.
            let sibling = destination_dir.join("photo.jpg");
            std::fs::write(&sibling, b"untouched").expect("sibling");
            let numbered = destination_dir.join("photo (1).jpg");
            let items = vec![PendingFileItem {
                source: object_ref(&source),
                destination_path: numbered.to_string_lossy().into_owned(),
                installed_path: None,
                undoable: true,
            }];

            let runtime = runtime_with_mapping(&destination_dir, transport);
            let job_id = JobId(job_id);
            runtime.track_running_job_for_test(job_id.clone(), items.len() as u64);
            let generation = runtime.cancel_generation(&job_id);
            let hook = install_batch_hook(&job_id, true);
            let batch_runtime = Arc::clone(&runtime);
            let batch_job_id = job_id.clone();
            let batch_items = items.clone();
            let batch = tokio::spawn(async move {
                batch_runtime
                    .try_rsync_batch_admitted(
                        &batch_job_id,
                        generation,
                        &batch_items,
                        false,
                        &ConflictResolution::Overwrite,
                    )
                    .await
            });

            // A valid source passes validation, so entering the hook proves the numbered
            // batch reached run_sftp_batch rather than being declined after preflight.
            tokio::time::timeout(Duration::from_secs(5), hook.entered.notified())
                .await
                .expect("numbered replay reached the SFTP mutation boundary");
            hook.release.notify_one();
            let result = tokio::time::timeout(Duration::from_secs(5), batch)
                .await
                .expect("batch settles")
                .expect("batch task joins");
            assert_eq!(result.expect("numbered replay dispatched"), Some(4));
            assert_eq!(
                std::fs::read(&numbered).expect("numbered destination written"),
                b"jpeg"
            );
            assert_eq!(
                std::fs::read(&sibling).expect("sibling intact"),
                b"untouched"
            );
            remove_batch_hook(&job_id);
        }

        assert_numbered_replay_dispatch(
            9_800_400,
            crate::remote_transport::RemoteTransportKind::Sftp,
        )
        .await;
        assert_numbered_replay_dispatch(
            9_800_500,
            crate::remote_transport::RemoteTransportKind::Auto,
        )
        .await;
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

    struct BatchFixture {
        _temp: tempfile::TempDir,
        runtime: Arc<AppRuntime>,
        job_id: JobId,
        generation: u64,
        items: Vec<PendingFileItem>,
        sources: Vec<PathBuf>,
        destinations: Vec<PathBuf>,
        contents: Vec<Vec<u8>>,
        is_move: bool,
    }

    fn batch_fixture(job_id: u64, is_move: bool) -> BatchFixture {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("src");
        let dest_dir = temp.path().join("dst");
        std::fs::create_dir_all(&source_dir).expect("source directory");
        std::fs::create_dir_all(&dest_dir).expect("destination directory");

        let contents: Vec<Vec<u8>> = vec![b"first payload".to_vec(), b"second payload".to_vec()];
        let mut items = Vec::new();
        let mut sources = Vec::new();
        let mut destinations = Vec::new();
        for (index, content) in contents.iter().enumerate() {
            let name = format!("file-{index}.bin");
            let source = source_dir.join(&name);
            let destination = dest_dir.join(&name);
            std::fs::write(&source, content).expect("source file");
            items.push(PendingFileItem {
                source: object_ref(&source),
                destination_path: destination.to_string_lossy().into_owned(),
                installed_path: None,
                undoable: true,
            });
            sources.push(source);
            destinations.push(destination);
        }

        let runtime = runtime_with_mapping(
            &dest_dir,
            crate::remote_transport::RemoteTransportKind::Sftp,
        );
        let job_id = JobId(job_id);
        runtime.track_running_job_for_test(job_id.clone(), items.len() as u64);
        let generation = runtime.cancel_generation(&job_id);
        BatchFixture {
            _temp: temp,
            runtime,
            job_id,
            generation,
            items,
            sources,
            destinations,
            contents,
            is_move,
        }
    }

    fn assert_no_batch_effect(fixture: &BatchFixture) {
        for (source, content) in fixture.sources.iter().zip(&fixture.contents) {
            assert_eq!(
                std::fs::read(source).expect("source intact").as_slice(),
                content.as_slice()
            );
        }
        for destination in &fixture.destinations {
            assert!(
                !destination.exists(),
                "unexpected write to {}",
                destination.display()
            );
        }
    }

    #[tokio::test]
    async fn stale_batch_dispatch_does_not_write_after_cancel_and_resume() {
        let fixture = batch_fixture(9_800_100, true);
        let hook = install_batch_hook(&fixture.job_id, true);
        let runtime = Arc::clone(&fixture.runtime);
        let job_id = fixture.job_id.clone();
        let items = fixture.items.clone();
        let generation = fixture.generation;
        let is_move = fixture.is_move;
        // The generation was captured before this future existed; admission must happen
        // on first poll, not when the dispatch was built.
        let dispatch = async move {
            runtime
                .try_rsync_batch_admitted(
                    &job_id,
                    generation,
                    &items,
                    is_move,
                    &ConflictResolution::Overwrite,
                )
                .await
        };

        fixture.runtime.cancel_job(&fixture.job_id);
        fixture.runtime.resume_job(&fixture.job_id);
        let result = tokio::time::timeout(Duration::from_secs(5), dispatch)
            .await
            .expect("stale dispatch settles")
            .expect_err("stale dispatch is rejected");
        assert_eq!(result, STALE_TRANSFER_DISPATCH);
        assert!(!hook.did_enter.load(std::sync::atomic::Ordering::SeqCst));
        assert_no_batch_effect(&fixture);
        assert!(!fixture.runtime.has_active_transfer(&fixture.job_id));
        remove_batch_hook(&fixture.job_id);
    }

    #[tokio::test]
    async fn cancel_after_batch_admission_prevents_writes_and_source_removal() {
        let fixture = batch_fixture(9_800_200, true);
        let hook = install_batch_hook(&fixture.job_id, true);
        let runtime = Arc::clone(&fixture.runtime);
        let job_id = fixture.job_id.clone();
        let items = fixture.items.clone();
        let generation = fixture.generation;
        let is_move = fixture.is_move;
        let batch = tokio::spawn(async move {
            runtime
                .try_rsync_batch_admitted(
                    &job_id,
                    generation,
                    &items,
                    is_move,
                    &ConflictResolution::Overwrite,
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), hook.entered.notified())
            .await
            .expect("batch reached the mutation boundary");
        assert!(fixture.runtime.has_active_transfer(&fixture.job_id));
        fixture.runtime.cancel_job(&fixture.job_id);
        hook.release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(5), batch)
            .await
            .expect("batch settles")
            .expect("batch task joins");
        assert_eq!(
            result.expect_err("canceled batch fails"),
            "transfer canceled"
        );
        assert_no_batch_effect(&fixture);
        assert!(!fixture.runtime.has_active_transfer(&fixture.job_id));
        remove_batch_hook(&fixture.job_id);
    }

    #[tokio::test]
    async fn uncanceled_admitted_batch_performs_effect() {
        let fixture = batch_fixture(9_800_300, true);
        let hook = install_batch_hook(&fixture.job_id, true);
        let runtime = Arc::clone(&fixture.runtime);
        let job_id = fixture.job_id.clone();
        let items = fixture.items.clone();
        let generation = fixture.generation;
        let is_move = fixture.is_move;
        let batch = tokio::spawn(async move {
            runtime
                .try_rsync_batch_admitted(
                    &job_id,
                    generation,
                    &items,
                    is_move,
                    &ConflictResolution::Overwrite,
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), hook.entered.notified())
            .await
            .expect("batch reached the mutation boundary");
        hook.release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(5), batch)
            .await
            .expect("batch settles")
            .expect("batch task joins");
        let total_bytes: u64 = fixture
            .contents
            .iter()
            .map(|content| content.len() as u64)
            .sum();
        assert_eq!(result.expect("batch effect succeeds"), Some(total_bytes));
        for (source, destination, content) in fixture
            .sources
            .iter()
            .zip(&fixture.destinations)
            .zip(&fixture.contents)
            .map(|((source, destination), content)| (source, destination, content))
        {
            assert!(!source.exists(), "move removed source {}", source.display());
            assert_eq!(
                std::fs::read(destination)
                    .expect("destination written")
                    .as_slice(),
                content.as_slice()
            );
        }
        assert!(!fixture.runtime.has_active_transfer(&fixture.job_id));
    }
}
