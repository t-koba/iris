use super::*;

/// Keeps a job's control reachable by Cancel for the entire admission and transfer.
/// The last item releases the registration, including on preflight failure or task abort.
struct TransferAdmission<'a> {
    runtime: &'a AppRuntime,
    job_id: u64,
    control: Option<Arc<TransferControl>>,
}

impl TransferAdmission<'_> {
    fn control(&self) -> &TransferControl {
        self.control.as_deref().expect("admission owns a control")
    }
}

impl Drop for TransferAdmission<'_> {
    fn drop(&mut self) {
        let mut active = self
            .runtime
            .active_transfers
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let control = self.control.take().expect("admission owns a control");
        // Two references means only the map and this item remain; clones use this lock.
        // An explicit resume may have replaced a canceled lifecycle; an old item must
        // not remove the new control.
        if active.get(&self.job_id).is_some_and(|registered| {
            Arc::ptr_eq(registered, &control) && Arc::strong_count(registered) == 2
        }) {
            active.remove(&self.job_id);
        }
        // Release our reference before unlocking, so concurrent rejections cannot both
        // leave the registration thinking another item still owns it.
        drop(control);
    }
}

struct TransferExecutionContext<'a> {
    job_id: &'a JobId,
    task_index: usize,
    record_chunk_state: bool,
    progress_tx: &'a watch::Sender<ProgressInfo>,
    control: &'a TransferControl,
    evidence: &'a DestinationWriteEvidence,
}

/// Tracks the strongest destination-write knowledge reached by one item. The state only
/// advances, so a cancellation can never downgrade a write that may already have happened.
#[derive(Clone, Default)]
struct DestinationWriteEvidence(Arc<Mutex<TransferWriteOutcome>>);

impl DestinationWriteEvidence {
    fn may_have_written(&self) {
        let mut evidence = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if *evidence == TransferWriteOutcome::NoWrite {
            *evidence = TransferWriteOutcome::MayHaveWritten;
        }
    }

    fn applied(&self, installed_path: Option<String>) {
        let mut evidence = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if !matches!(
            &*evidence,
            TransferWriteOutcome::Applied {
                installed_path: Some(_)
            }
        ) {
            *evidence = TransferWriteOutcome::Applied { installed_path };
        }
    }

    fn failure(&self, error: String) -> TransferFailure {
        if error == "transfer canceled" {
            TransferFailure::Canceled(
                self.0
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone(),
            )
        } else {
            TransferFailure::Error(error)
        }
    }
}

struct TransferPathRequest<'a> {
    source: &'a ObjectRef,
    destination: &'a str,
    is_move: bool,
    resolution: ConflictResolution,
}

struct TransferDirectoryRequest<'a> {
    source: &'a ObjectRef,
    destination: &'a Path,
    is_move: bool,
    resolution: ConflictResolution,
}

/// Resolve aliases without requiring the destination to exist. Only append normal missing
/// components; dangling links, inaccessible ancestors and ambiguous `..` paths fail closed.
async fn canonicalize_destination(path: &Path) -> Result<PathBuf, String> {
    let mut ancestor = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match tokio::fs::canonicalize(&ancestor).await {
            Ok(mut resolved) => {
                for component in missing.into_iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound
                    || !tokio::fs::symlink_metadata(&ancestor)
                        .await
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                {
                    return Err(format!("Cannot verify directory transfer safety: {error}"));
                }
                let Some(std::path::Component::Normal(name)) = ancestor.components().next_back()
                else {
                    return Err(format!(
                        "Cannot verify directory transfer safety: {}",
                        path.display()
                    ));
                };
                missing.push(name.to_os_string());
                ancestor.pop();
                if ancestor.as_os_str().is_empty() {
                    ancestor.push(".");
                }
            }
        }
    }
}

enum SymlinkTransfer {
    /// Not a local symlink, or the destination cannot hold one: copy what it points to.
    NotHandled,
    /// The link itself was recreated at the destination.
    Recreated,
    /// Following it would copy an ancestor of itself, recursing without end.
    LinksToAncestor,
}

impl AppRuntime {
    /// Preflight real directories before starting a transfer (including external batch tools).
    /// Symlinks are copied as links; any fallback that traverses their target is checked below.
    pub(super) async fn ensure_safe_transfer_destination(
        &self,
        source: &ObjectRef,
        destination: &Path,
    ) -> Result<(), String> {
        source
            .ensure_operable()
            .map_err(|error| error.to_string())?;
        let is_directory = if !self.is_virtual_display_path(&source.display_path) {
            tokio::fs::symlink_metadata(&source.display_path)
                .await
                .map_err(|error| error.to_string())?
                .is_dir()
        } else {
            self.is_directory_object(source).await?
        };
        if is_directory {
            self.ensure_directory_destination_outside_source(source, destination)
                .await?;
        }
        Ok(())
    }

    async fn ensure_directory_destination_outside_source(
        &self,
        source: &ObjectRef,
        destination: &Path,
    ) -> Result<(), String> {
        let src_path = Path::new(&source.display_path);
        let contained = if !self.is_virtual_display_path(&source.display_path)
            && !self.is_virtual_display_path(destination.to_string_lossy().as_ref())
        {
            let canonical_source = tokio::fs::canonicalize(src_path)
                .await
                .map_err(|error| format!("Cannot verify directory transfer safety: {error}"))?;
            let canonical_destination = canonicalize_destination(destination).await?;
            // Check the entry's parent as well: an existing symlink must not hide that the
            // entry to be replaced lives inside the source, even if its target is elsewhere.
            let parent_is_contained = if matches!(
                destination.components().next_back(),
                Some(std::path::Component::Normal(_))
            ) {
                let parent = destination
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                canonicalize_destination(parent)
                    .await?
                    .starts_with(&canonical_source)
            } else {
                false
            };
            canonical_destination.starts_with(&canonical_source) || parent_is_contained
        } else {
            let destination_backend = self.backend_id_for_path(destination);
            let same_backend = source.key.backend_id == destination_backend
                || self.backend_id_for_path(src_path) == destination_backend;
            same_backend
                && (destination.starts_with(src_path)
                    || destination
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir)))
        };
        if contained {
            return Err(
                "Cannot copy or move a directory into itself or one of its descendants".into(),
            );
        }
        Ok(())
    }

    /// Copies keep symlinks as symlinks where the destination can hold them, as `cp -a` and
    /// Finder do. Following them would duplicate the target, and a link to an ancestor (Wine's
    /// `dosdevices/z: -> /`) would copy the whole disk. Renames are left to the move path, which
    /// moves the link itself.
    async fn transfer_local_symlink(
        &self,
        source: &ObjectRef,
        destination: &Path,
        is_move: bool,
        resolution: &ConflictResolution,
    ) -> Result<SymlinkTransfer, String> {
        if self.is_virtual_display_path(&source.display_path) {
            return Ok(SymlinkTransfer::NotHandled);
        }
        let src_path = PathBuf::from(&source.display_path);
        let is_symlink = tokio::fs::symlink_metadata(&src_path)
            .await
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        if !is_symlink {
            return Ok(SymlinkTransfer::NotHandled);
        }
        let src_resolved = self.resolve_backend(&src_path);
        let dst_resolved = self.resolve_backend(destination);
        if is_move && self.same_backend(&src_path, &src_resolved, destination, &dst_resolved) {
            return Ok(SymlinkTransfer::NotHandled);
        }

        if !self.is_virtual_display_path(destination.to_string_lossy().as_ref()) {
            let (link, dst) = (src_path.clone(), destination.to_path_buf());
            let overwrite = matches!(resolution, ConflictResolution::Overwrite);
            let recreated =
                tokio::task::spawn_blocking(move || recreate_symlink(&link, &dst, overwrite))
                    .await
                    .map_err(|error| error.to_string())?;
            match recreated {
                Ok(()) => {
                    if is_move {
                        remove_symlink(&src_path)
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    return Ok(SymlinkTransfer::Recreated);
                }
                // A conflict (including an entry appearing after preflight) is not a
                // lack of symlink support: never fall back to copying through that entry.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(error.to_string());
                }
                Err(error) => tracing::debug!(
                    "destination cannot hold a symlink ({error}); copying its target instead"
                ),
            }
        }

        let target = tokio::fs::canonicalize(&src_path).await;
        let parent = match src_path.parent() {
            Some(parent) => tokio::fs::canonicalize(parent).await,
            None => return Ok(SymlinkTransfer::NotHandled),
        };
        if let (Ok(target), Ok(parent)) = (target, parent) {
            if parent.starts_with(&target) {
                return Ok(SymlinkTransfer::LinksToAncestor);
            }
        }
        Ok(SymlinkTransfer::NotHandled)
    }

    #[cfg(test)]
    fn admit_transfer(&self, job_id: &JobId) -> Result<TransferAdmission<'_>, String> {
        self.admit_transfer_for(job_id, self.job_manager.cancel_generation(job_id))
    }

    fn admit_transfer_for(
        &self,
        job_id: &JobId,
        dispatch_generation: u64,
    ) -> Result<TransferAdmission<'_>, String> {
        let mut active = self
            .active_transfers
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Cancel changes the state and generation under this same lock. Resume can
        // change the state back, but dispatches from before Cancel still cannot register.
        let state = self.job_manager.get_state(job_id);
        if state == Some(JobState::Canceled) {
            return Err("transfer canceled".into());
        }
        if self.job_manager.cancel_generation(job_id) != dispatch_generation {
            return Err(STALE_TRANSFER_DISPATCH.into());
        }
        let control = active
            .entry(job_id.0)
            .or_insert_with(|| {
                let control = Arc::new(TransferControl::new());
                // Registrations live only while items are in flight; preserve Pause between
                // items as well as across concurrent admissions.
                if state == Some(JobState::Paused) {
                    control.pause();
                }
                control
            })
            .clone();
        Ok(TransferAdmission {
            runtime: self,
            job_id: job_id.0,
            control: Some(control),
        })
    }

    /// Captures the job's cancellation generation before the returned future is polled.
    pub fn apply_pending_item(
        self: Arc<Self>,
        job_id: JobId,
        task_index: usize,
        item: PendingFileItem,
        is_move: bool,
        resolution: ConflictResolution,
        base_bytes_completed: u64,
    ) -> impl std::future::Future<Output = Result<TransferItemOutcome, TransferFailure>> + Send + 'static
    {
        let dispatch_generation = self.job_manager.cancel_generation(&job_id);
        async move {
            let result = self
                .clone()
                .apply_pending_item_inner(
                    job_id.clone(),
                    task_index,
                    item,
                    is_move,
                    resolution,
                    base_bytes_completed,
                    dispatch_generation,
                )
                .await;
            // An admitted dispatch keeps its canceled control even after the job resumes.
            match result {
                Err(TransferFailure::Canceled(_))
                    if self.job_manager.cancel_generation(&job_id) != dispatch_generation
                        && self.job_manager.get_state(&job_id) != Some(JobState::Canceled) =>
                {
                    Err(TransferFailure::Error(STALE_TRANSFER_DISPATCH.to_string()))
                }
                result => result,
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Carries the captured generation alongside the transfer arguments"
    )]
    async fn apply_pending_item_inner(
        self: Arc<Self>,
        job_id: JobId,
        task_index: usize,
        item: PendingFileItem,
        is_move: bool,
        resolution: ConflictResolution,
        base_bytes_completed: u64,
        dispatch_generation: u64,
    ) -> Result<TransferItemOutcome, TransferFailure> {
        #[cfg(test)]
        tests::pause_admission(&job_id, tests::AdmissionStage::BeforeRegistration).await;

        let evidence = DestinationWriteEvidence::default();
        let admission = self
            .admit_transfer_for(&job_id, dispatch_generation)
            .map_err(|error| evidence.failure(error))?;
        let control = admission.control();
        #[cfg(test)]
        tests::pause_admission(&job_id, tests::AdmissionStage::Preflight).await;

        let preflight = self
            .ensure_safe_transfer_destination(&item.source, Path::new(&item.destination_path))
            .await;
        // Cancellation wins even if preflight also failed. Never look up (or recreate) a
        // control after the await: Cancel may have removed it, but this item still owns it.
        if control.cancel_token().is_cancelled()
            || self.job_manager.get_state(&job_id) == Some(JobState::Canceled)
        {
            return Err(evidence.failure("transfer canceled".to_string()));
        }
        preflight.map_err(|error| evidence.failure(error))?;
        let task_id = persisted_task_id(&job_id, task_index);

        let (progress_tx, mut progress_rx) =
            watch::channel(ProgressInfo::single_file(0, None, false));

        let progress_runtime = Arc::clone(&self);
        let progress_job_id = job_id.clone();
        let progress_task_id = task_id.clone();
        let progress_forwarder = tokio::spawn(async move {
            while progress_rx.changed().await.is_ok() {
                let progress = progress_rx.borrow().clone();
                let aggregate_bytes = progress_runtime.update_active_job_progress(
                    &progress_job_id,
                    &progress_task_id,
                    base_bytes_completed,
                    progress.bytes_completed,
                );
                progress_runtime
                    .job_manager
                    .update_progress(&progress_job_id, aggregate_bytes);
                progress_runtime.update_task_progress(
                    &progress_job_id,
                    &progress_task_id,
                    progress.bytes_completed,
                );
            }
        });

        let context = TransferExecutionContext {
            job_id: &job_id,
            task_index,
            record_chunk_state: true,
            progress_tx: &progress_tx,
            control,
            evidence: &evidence,
        };
        let transfer_result = self
            .transfer_path(
                &context,
                TransferPathRequest {
                    source: &item.source,
                    destination: &item.destination_path,
                    is_move,
                    resolution,
                },
            )
            .await;

        drop(progress_tx);
        let _ = progress_forwarder.await;

        let aggregate_bytes = match &transfer_result {
            Ok(outcome) => self.finalize_active_job_progress(
                &job_id,
                &task_id,
                base_bytes_completed,
                Some(outcome.bytes_completed),
            ),
            Err(_) => {
                self.finalize_active_job_progress(&job_id, &task_id, base_bytes_completed, None)
            }
        };
        self.job_manager.update_progress(&job_id, aggregate_bytes);

        match transfer_result {
            Ok(outcome) => {
                if outcome.applied {
                    self.update_task_state_with_installed_path(
                        &job_id,
                        task_index,
                        TaskState::Completed,
                        None,
                        if outcome.undoable {
                            outcome.installed_path.as_deref()
                        } else {
                            None
                        },
                    );
                } else {
                    self.skip_task(&job_id, task_index);
                }
                Ok(outcome)
            }
            Err(error) => {
                self.update_task_state(&job_id, task_index, TaskState::Failed, Some(&error));
                Err(evidence.failure(error))
            }
        }
    }

    fn transfer_path<'a>(
        &'a self,
        context: &'a TransferExecutionContext<'a>,
        request: TransferPathRequest<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TransferItemOutcome, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            let job_id = context.job_id;
            let task_index = context.task_index;
            let control = context.control;
            let evidence = context.evidence;
            let TransferPathRequest {
                source,
                destination,
                is_move,
                resolution,
            } = request;
            source
                .ensure_operable()
                .map_err(|error| error.to_string())?;
            let dst_path = Path::new(destination);
            wait_if_paused_or_canceled(control).await?;
            if self.job_manager.get_state(job_id) != Some(JobState::Running) {
                self.wait_for_backend_slots(
                    job_id,
                    control,
                    &[
                        backend_type_from_id(&source.key.backend_id),
                        backend_type_from_id(&self.backend_id_for_path(dst_path)),
                    ],
                )
                .await?;

                self.set_transfer_job_state(job_id, control, JobState::Running)?;
                self.update_task_state(job_id, task_index, TaskState::Running, None);
            }

            if matches!(resolution, ConflictResolution::Skip)
                && self.destination_entry_exists(dst_path).await?
            {
                self.update_task_state(job_id, task_index, TaskState::Skipped, None);
                return Ok(TransferItemOutcome {
                    bytes_completed: 0,
                    applied: false,
                    installed_path: None,
                    undoable: false,
                });
            }

            let final_dst = self
                .resolve_destination_path_async(dst_path, &resolution)
                .await?;
            // Record the exact installed path, including when it equals the requested
            // destination: a completion without this proof cannot support Undo later.
            let installed_path = Some(final_dst.to_string_lossy().into_owned());
            // Past this point an installed symlink, directory, child or file may write to the
            // destination. A cancellation from here on is never proof of a no-write replay.
            evidence.may_have_written();

            match self
                .transfer_local_symlink(source, &final_dst, is_move, &resolution)
                .await?
            {
                SymlinkTransfer::Recreated => {
                    evidence.applied(installed_path.clone());
                    return Ok(TransferItemOutcome {
                        bytes_completed: 0,
                        applied: true,
                        installed_path,
                        undoable: true,
                    });
                }
                SymlinkTransfer::LinksToAncestor => {
                    // Nothing was copied; undoing it would target an unrelated destination.
                    return Ok(TransferItemOutcome {
                        bytes_completed: 0,
                        applied: false,
                        installed_path: None,
                        undoable: false,
                    });
                }
                SymlinkTransfer::NotHandled => {}
            }

            if self.is_directory_object(source).await? {
                return self
                    .transfer_directory(
                        context,
                        TransferDirectoryRequest {
                            source,
                            destination: &final_dst,
                            is_move,
                            resolution,
                        },
                    )
                    .await
                    .map(|mut outcome| {
                        if outcome.applied {
                            evidence.applied(installed_path.clone());
                            outcome.installed_path = installed_path;
                        }
                        outcome
                    });
            }

            let installed = self
                .transfer_file(context, source, &final_dst, is_move, resolution)
                .await
                .inspect(|_| evidence.applied(installed_path.clone()));
            // Test-only: inject a cancellation observed after the real destination write.
            #[cfg(test)]
            let installed = tests::cancel_after_destination_write(job_id, installed);
            installed.map(|result| TransferItemOutcome {
                bytes_completed: result.bytes_transferred,
                applied: true,
                installed_path,
                undoable: true,
            })
        })
    }

    fn transfer_directory<'a>(
        &'a self,
        context: &'a TransferExecutionContext<'a>,
        request: TransferDirectoryRequest<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TransferItemOutcome, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            let control = context.control;
            let child_context = TransferExecutionContext {
                job_id: context.job_id,
                task_index: context.task_index,
                record_chunk_state: false,
                progress_tx: context.progress_tx,
                control,
                evidence: context.evidence,
            };
            let TransferDirectoryRequest {
                source,
                destination,
                is_move,
                resolution,
            } = request;
            self.ensure_directory_destination_outside_source(source, destination)
                .await?;
            wait_if_paused_or_canceled(control).await?;
            let src_path = Path::new(&source.display_path);
            let src_resolved = self.resolve_backend(src_path);
            let dst_resolved = self.resolve_backend(destination);
            let same_backend =
                self.same_backend(src_path, &src_resolved, destination, &dst_resolved);
            let high_risk_path =
                is_high_risk_path(&src_resolved, &dst_resolved, src_path, destination);

            if is_move
                && same_backend
                && matches!(
                    select_move_method(
                        &src_resolved.adapter.capabilities(),
                        &dst_resolved.adapter.capabilities(),
                        true,
                    ),
                    MoveMethod::RenameMove
                )
            {
                let src_ref = self.source_object_ref(source, &src_resolved)?;
                let dst_ref = self.object_ref_for_path(destination, &dst_resolved)?;
                self.rename_move(
                    &src_resolved,
                    &src_ref,
                    &dst_ref,
                    &source.display_path,
                    destination,
                    &resolution,
                )
                .await?;
                return Ok(TransferItemOutcome {
                    bytes_completed: 0,
                    applied: true,
                    installed_path: None,
                    undoable: true,
                });
            }

            let created = self
                .create_directory_if_missing(&dst_resolved, destination)
                .await?;
            // Only a directory this copy created takes the source's time; merging into an
            // existing directory leaves that directory's metadata to the file system.
            let source_mtime = match (created, self.source_object_ref(source, &src_resolved)) {
                (true, Ok(src_ref)) => src_resolved
                    .adapter
                    .stat(&src_ref)
                    .await
                    .ok()
                    .and_then(|metadata| metadata.mtime),
                _ => None,
            };

            let mut total_bytes = 0u64;
            let mut all_children_applied = true;
            for child in self.list_directory_children(source, &src_resolved).await? {
                child
                    .object_ref
                    .ensure_operable()
                    .map_err(|error| error.to_string())?;
                let child_dst = destination.join(&child.name);
                let child_dst_string = child_dst.to_string_lossy().to_string();
                let outcome = self
                    .transfer_path(
                        &child_context,
                        TransferPathRequest {
                            source: &child.object_ref,
                            destination: &child_dst_string,
                            is_move: false,
                            resolution: resolution.clone(),
                        },
                    )
                    .await?;
                total_bytes += outcome.bytes_completed;
                all_children_applied &= outcome.applied;
            }

            // Creating the directory and its children stamped it with the current time.
            if let Some(mtime) = source_mtime {
                self.preserve_directory_mtime(destination, &dst_resolved, mtime)
                    .await;
            }

            if is_move {
                let dst_ref = self.object_ref_for_path(destination, &dst_resolved)?;
                verify_before_move_delete(control, async {
                    if src_resolved.kind == VolumeKind::Local
                        && dst_resolved.kind == VolumeKind::Local
                        && !self.is_virtual_display_path(&source.display_path)
                        && !self.is_virtual_display_path(destination.to_string_lossy().as_ref())
                    {
                        verify_directory_mirror(src_path, destination, high_risk_path).await?;
                    } else {
                        self.verify_directory_copy(
                            source,
                            &src_resolved,
                            &dst_ref,
                            &dst_resolved,
                            high_risk_path,
                        )
                        .await?;
                    }
                    Ok(())
                })
                .await?;
                self.delete_object(source, false).await?;
            }

            Ok(TransferItemOutcome {
                bytes_completed: total_bytes,
                // A partially skipped directory cannot be undone as a whole: Undo would
                // delete the destination the user chose to keep.
                applied: all_children_applied,
                installed_path: None,
                // A merged folder is not owned as a whole: Undo would delete the user's
                // pre-existing children along with the transferred ones.
                undoable: created && all_children_applied,
            })
        })
    }

    async fn transfer_file(
        &self,
        context: &TransferExecutionContext<'_>,
        src: &ObjectRef,
        dst: &Path,
        is_move: bool,
        resolution: ConflictResolution,
    ) -> Result<TransferResult, String> {
        let progress_tx = context.progress_tx;
        let control = context.control;
        wait_if_paused_or_canceled(control).await?;
        let src_path = Path::new(&src.display_path);

        let src_resolved = self.resolve_backend(src_path);
        let dst_resolved = self.resolve_backend(dst);
        let src_ref = self.source_object_ref(src, &src_resolved)?;
        let dst_ref = self.object_ref_for_path(dst, &dst_resolved)?;
        let same_backend = self.same_backend(src_path, &src_resolved, dst, &dst_resolved);
        let high_risk_path = is_high_risk_path(&src_resolved, &dst_resolved, src_path, dst);
        let started_at = std::time::Instant::now();
        let resumable = context
            .record_chunk_state
            .then(|| ResumableTransferContext {
                job_store: Arc::clone(&self.job_store),
                job_id: context.job_id.clone(),
                entry_index: context.task_index as u64,
            });

        if is_move {
            match select_move_method(
                &src_resolved.adapter.capabilities(),
                &dst_resolved.adapter.capabilities(),
                same_backend,
            ) {
                MoveMethod::RenameMove => {
                    self.rename_move(
                        &src_resolved,
                        &src_ref,
                        &dst_ref,
                        &src.display_path,
                        dst,
                        &resolution,
                    )
                    .await?;
                    return self
                        .finish_server_side_transfer(
                            dst,
                            progress_tx,
                            &src_resolved,
                            &dst_resolved,
                            started_at,
                        )
                        .await;
                }
                MoveMethod::ServerSideCopyThenDelete => {
                    src_resolved
                        .adapter
                        .copy(
                            &src_ref,
                            &dst_ref,
                            CopyOptions {
                                on_conflict: resolution.clone(),
                            },
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    verify_and_delete_move(
                        src_resolved.adapter.as_ref(),
                        &src_ref,
                        dst_resolved.adapter.as_ref(),
                        &dst_ref,
                        high_risk_path,
                        control,
                    )
                    .await?;
                    self.reconcile_cache_after_move(src, &src_resolved, dst, &dst_resolved)
                        .await;
                    return self
                        .finish_server_side_transfer(
                            dst,
                            progress_tx,
                            &src_resolved,
                            &dst_resolved,
                            started_at,
                        )
                        .await;
                }
                MoveMethod::ClientRelayThenDelete => {
                    let result = move_via_relay_with_verify_and_control(
                        src_resolved.adapter.as_ref(),
                        &src_ref,
                        dst_resolved.adapter.as_ref(),
                        &dst_ref,
                        progress_tx,
                        MoveRelayOptions {
                            control: Some(control),
                            resumable: resumable.as_ref(),
                            user_requested_strong_verify: false,
                            high_risk_path,
                            retry_policy: None,
                        },
                    )
                    .await
                    .map_err(relay_move_error)?;

                    self.reconcile_cache_after_move(src, &src_resolved, dst, &dst_resolved)
                        .await;
                    record_transfer_sample(
                        &src_resolved,
                        &dst_resolved,
                        result.bytes_transferred,
                        started_at,
                    );
                    return Ok(result);
                }
            }
        }

        match select_copy_method(
            &src_resolved.adapter.capabilities(),
            &dst_resolved.adapter.capabilities(),
            same_backend,
        ) {
            CopyMethod::CloneCopy | CopyMethod::ServerSideCopy => {
                src_resolved
                    .adapter
                    .copy(
                        &src_ref,
                        &dst_ref,
                        CopyOptions {
                            on_conflict: resolution,
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                self.finish_server_side_transfer(
                    dst,
                    progress_tx,
                    &src_resolved,
                    &dst_resolved,
                    started_at,
                )
                .await
            }
            CopyMethod::ClientRelayCopy => {
                let result = client_relay_copy(
                    src_resolved.adapter.as_ref(),
                    &src_ref,
                    dst_resolved.adapter.as_ref(),
                    &dst_ref,
                    progress_tx,
                    CopyRelayOptions {
                        control: Some(control),
                        resumable: resumable.as_ref(),
                        retry_policy: None,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;

                record_transfer_sample(
                    &src_resolved,
                    &dst_resolved,
                    result.bytes_transferred,
                    started_at,
                );
                Ok(result)
            }
        }
    }

    /// Moves within one backend by renaming, keeping cached listings under the new path.
    async fn rename_move(
        &self,
        src_resolved: &ResolvedBackend,
        src_ref: &ObjectRef,
        dst_ref: &ObjectRef,
        src_display_path: &str,
        dst: &Path,
        resolution: &ConflictResolution,
    ) -> Result<(), String> {
        src_resolved
            .adapter
            .move_object(
                src_ref,
                dst_ref,
                MoveOptions {
                    on_conflict: resolution.clone(),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        self.rebind_cached_display_path_prefix(
            &src_resolved.backend_id,
            src_display_path,
            &dst.to_string_lossy(),
        );
        Ok(())
    }

    /// Reports a transfer done by the storage service: the size comes from the destination.
    async fn finish_server_side_transfer(
        &self,
        dst: &Path,
        progress_tx: &watch::Sender<ProgressInfo>,
        src_resolved: &ResolvedBackend,
        dst_resolved: &ResolvedBackend,
        started_at: std::time::Instant,
    ) -> Result<TransferResult, String> {
        let total_bytes = self
            .stat_path(dst.to_string_lossy().as_ref())
            .await?
            .and_then(|meta| meta.size)
            .unwrap_or(0);
        record_transfer_sample(src_resolved, dst_resolved, total_bytes, started_at);
        Ok(iris_transfer::executor::finish_file(
            progress_tx,
            total_bytes,
        ))
    }

    async fn preserve_directory_mtime(
        &self,
        destination: &Path,
        dst_resolved: &ResolvedBackend,
        mtime: SystemTime,
    ) {
        let Ok(dst_ref) = self.object_ref_for_path(destination, dst_resolved) else {
            return;
        };
        match dst_resolved.adapter.set_mtime(&dst_ref, mtime).await {
            Ok(()) | Err(BackendError::Unsupported { .. }) => {}
            Err(error) => tracing::warn!(
                path_hash = %iris_core_domain::observability::path_hash(&dst_ref.display_path),
                "failed to preserve the source directory modification time: {error}"
            ),
        }
    }

    pub(super) async fn wait_for_backend_slots(
        &self,
        job_id: &JobId,
        control: &TransferControl,
        backend_types: &[String],
    ) -> Result<(), String> {
        let unique_backend_types = backend_types
            .iter()
            .filter(|backend_type| !backend_type.is_empty())
            .fold(Vec::<String>::new(), |mut kinds, backend_type| {
                if !kinds.contains(backend_type) {
                    kinds.push((*backend_type).clone());
                }
                kinds
            });

        while unique_backend_types
            .iter()
            .any(|backend_type| !self.job_manager.can_start_job(backend_type))
        {
            wait_if_paused_or_canceled(control).await?;
            self.set_transfer_job_state(job_id, control, JobState::WaitingRemote)?;
            sleep(Duration::from_millis(50)).await;
        }

        Ok(())
    }
}

/// Remote ends or a different parent folder call for verification before the source goes.
fn is_high_risk_path(
    src_resolved: &ResolvedBackend,
    dst_resolved: &ResolvedBackend,
    src: &Path,
    dst: &Path,
) -> bool {
    src_resolved.kind != VolumeKind::Local
        || dst_resolved.kind != VolumeKind::Local
        || src.parent() != dst.parent()
}

fn record_transfer_sample(
    src: &ResolvedBackend,
    dst: &ResolvedBackend,
    bytes_transferred: u64,
    started_at: std::time::Instant,
) {
    let elapsed = started_at.elapsed();
    let mut observed = Vec::<&str>::new();
    for backend_id in [&src.backend_id.0, &dst.backend_id.0] {
        let backend_type = backend_id.split(':').next().unwrap_or(backend_id);
        if observed.contains(&backend_type) {
            continue;
        }
        observed.push(backend_type);
        let _ = iris_transfer::method_selector::observe_backend_sample(
            backend_type,
            elapsed,
            bytes_transferred,
        );
    }
}

/// Creates at `dst` a symlink with the same target as `link`. An existing file or link at `dst`
/// is replaced only with explicit overwrite authorization; a directory is never removed.
fn recreate_symlink(link: &Path, dst: &Path, overwrite: bool) -> std::io::Result<()> {
    let target = std::fs::read_link(link)?;
    recreate_symlink_with(
        dst,
        overwrite,
        |destination| {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&target, destination)
            }
            #[cfg(windows)]
            {
                // Windows needs to know the kind; creating links needs Developer Mode or elevation.
                if std::fs::metadata(link).is_ok_and(|metadata| metadata.is_dir()) {
                    std::os::windows::fs::symlink_dir(&target, destination)
                } else {
                    std::os::windows::fs::symlink_file(&target, destination)
                }
            }
            #[cfg(not(any(unix, windows)))]
            {
                let _ = (&target, destination);
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "symlinks are unsupported on this platform",
                ))
            }
        },
        |staged, destination| std::fs::rename(staged, destination),
    )
}

/// `create` must atomically create a new link or leave its path untouched on error, just
/// like the platform symlink APIs. In particular, AlreadyExists never grants ownership.
fn recreate_symlink_with(
    dst: &Path,
    overwrite: bool,
    create: impl Fn(&Path) -> std::io::Result<()>,
    replace: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    if overwrite {
        match std::fs::symlink_metadata(dst) {
            Ok(existing) if existing.is_dir() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "a directory already exists at the destination",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let parent = dst
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    // Without explicit Overwrite, creation itself atomically rejects any occupied entry.
    if !overwrite {
        return create(dst);
    }

    static NEXT_LINK: AtomicU64 = AtomicU64::new(0);
    let staged = loop {
        let candidate = parent.join(format!(
            ".iris-link-{}-{}",
            std::process::id(),
            NEXT_LINK.fetch_add(1, Ordering::Relaxed)
        ));
        match create(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };

    // Same-filesystem replace-rename: Unix rename / Windows MoveFileExW with
    // REPLACE_EXISTING (via std). If Windows refuses replacement (e.g. a sharing lock
    // or directory link), fail closed, never unlink or move the original aside. This
    // avoids a fallible backup/restore window altogether. A directory racing with
    // our earlier check also cannot be replaced by a non-directory link on Unix.
    match replace(&staged, dst) {
        Ok(()) => Ok(()),
        Err(error) => {
            // Only this successfully created entry belongs to us; never remove a collision.
            remove_symlink_sync(&staged)?;
            // A conflicting entry must not turn a failed commit into target-copy fallback.
            if std::fs::symlink_metadata(dst).is_ok() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    error,
                ))
            } else {
                Err(error)
            }
        }
    }
}

fn remove_symlink_sync(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if cfg!(windows) => std::fs::remove_dir(path).map_err(|_| error),
        result => result,
    }
}

/// Removes the link, never what it points to (Windows directory links need `remove_dir`).
async fn remove_symlink(path: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(error) if cfg!(windows) => tokio::fs::remove_dir(path).await.map_err(|_| error),
        result => result,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    static CANCEL_ON_WRITE: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();

    /// Marks a job whose next completed destination write must be reported as canceled. The
    /// write itself is performed by the real transfer; only the observed cancellation is
    /// injected, so completion and journal handling are exercised unchanged.
    pub(crate) fn cancel_after_destination_write(
        job_id: &JobId,
        result: Result<TransferResult, String>,
    ) -> Result<TransferResult, String> {
        let cancel = CANCEL_ON_WRITE
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .remove(&job_id.0);
        if cancel && result.is_ok() {
            Err("transfer canceled".to_string())
        } else {
            result
        }
    }

    pub(crate) fn install_cancel_after_destination_write(job_id: &JobId) {
        CANCEL_ON_WRITE
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(job_id.0);
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum AdmissionStage {
        BeforeRegistration,
        Preflight,
    }

    struct AdmissionHook {
        stage: AdmissionStage,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    static ADMISSION_HOOKS: OnceLock<Mutex<HashMap<u64, Arc<AdmissionHook>>>> = OnceLock::new();

    impl AdmissionHook {
        fn install(job_id: &JobId, stage: AdmissionStage) -> Arc<Self> {
            let hook = Arc::new(Self {
                stage,
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            });
            ADMISSION_HOOKS
                .get_or_init(Default::default)
                .lock()
                .unwrap()
                .insert(job_id.0, Arc::clone(&hook));
            hook
        }
    }

    pub(super) async fn pause_admission(job_id: &JobId, stage: AdmissionStage) {
        let hook = {
            let mut hooks = ADMISSION_HOOKS
                .get_or_init(Default::default)
                .lock()
                .unwrap();
            if hooks.get(&job_id.0).is_some_and(|hook| hook.stage == stage) {
                hooks.remove(&job_id.0)
            } else {
                None
            }
        };
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }

    fn has_active_transfer(job_id: &JobId) -> bool {
        app_runtime()
            .active_transfers
            .lock()
            .unwrap()
            .contains_key(&job_id.0)
    }

    async fn assert_canceled_admission(stage: AdmissionStage, first_job_id: u64) {
        let runtime = app_runtime();
        for is_move in [false, true] {
            for existing_destination in [false, true] {
                let job_id =
                    JobId(first_job_id + u64::from(is_move) * 2 + u64::from(existing_destination));
                runtime
                    .job_manager
                    .track_job(job_id.clone(), JobState::Running, 1, None, vec![]);
                let temp = tempfile::tempdir().unwrap();
                let source = temp.path().join("source");
                let destination = temp.path().join("destination");
                std::fs::write(&source, b"source content").unwrap();
                if existing_destination {
                    std::fs::write(&destination, b"destination unchanged").unwrap();
                }
                // The later-item case already has an admitted item sharing the control.
                let admitted = (stage == AdmissionStage::Preflight)
                    .then(|| runtime.admit_transfer(&job_id).unwrap());
                let hook = AdmissionHook::install(&job_id, stage);
                let pending = tokio::spawn(Arc::clone(runtime).apply_pending_item(
                    job_id.clone(),
                    0,
                    PendingFileItem {
                        source: local_ref(&source),
                        destination_path: destination.to_str().unwrap().to_owned(),
                        installed_path: None,
                        undoable: true,
                    },
                    is_move,
                    ConflictResolution::Overwrite,
                    0,
                ));
                tokio::time::timeout(StdDuration::from_secs(10), hook.entered.notified())
                    .await
                    .unwrap();
                assert_eq!(has_active_transfer(&job_id), admitted.is_some());
                runtime.cancel_job(&job_id);
                if let Some(admitted) = &admitted {
                    assert!(admitted.control().cancel_token().is_cancelled());
                }
                hook.release.notify_one();
                let error = tokio::time::timeout(StdDuration::from_secs(10), pending)
                    .await
                    .unwrap()
                    .unwrap()
                    .expect_err("canceled admission must not transfer");
                assert_eq!(
                    error,
                    TransferFailure::Canceled(TransferWriteOutcome::NoWrite)
                );
                assert_eq!(
                    runtime.job_manager.get_state(&job_id),
                    Some(JobState::Canceled)
                );
                assert!(!has_active_transfer(&job_id));
                assert_eq!(
                    runtime.admit_transfer(&job_id).err().unwrap(),
                    "transfer canceled"
                );
                // An absent destination stays absent, and an overwrite target stays byte-exact:
                // neither a destination write nor a move's source deletion occurred.
                assert_eq!(std::fs::read(&source).unwrap(), b"source content");
                if existing_destination {
                    assert_eq!(
                        std::fs::read(&destination).unwrap(),
                        b"destination unchanged"
                    );
                } else {
                    assert!(!destination.exists());
                }
                assert_eq!(
                    std::fs::read_dir(temp.path()).unwrap().count(),
                    1 + usize::from(existing_destination)
                );
            }
        }
    }

    #[tokio::test]
    async fn cancel_before_first_transfer_registration_prevents_writes_and_source_deletion() {
        // Exercise the old preflight window where no control existed yet. Real preflight
        // now follows registration, so the hook holds admission immediately before it.
        assert_canceled_admission(AdmissionStage::BeforeRegistration, 9_100_000).await;
    }

    #[tokio::test]
    async fn cancel_during_transfer_preflight_prevents_control_resurrection_and_writes() {
        assert_canceled_admission(AdmissionStage::Preflight, 9_100_010).await;
    }

    #[tokio::test]
    async fn cancel_then_resume_rejects_a_stale_dispatched_item() {
        let runtime = app_runtime();
        let job_id = JobId(9_100_030);
        runtime
            .job_manager
            .track_job(job_id.clone(), JobState::Running, 1, None, vec![]);
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::write(&source, b"source content").unwrap();

        let hook = AdmissionHook::install(&job_id, AdmissionStage::BeforeRegistration);

        let dispatch = || {
            Arc::clone(runtime).apply_pending_item(
                job_id.clone(),
                0,
                PendingFileItem {
                    source: local_ref(&source),
                    destination_path: destination.to_str().unwrap().to_owned(),
                    installed_path: None,
                    undoable: true,
                },
                false,
                ConflictResolution::Overwrite,
                0,
            )
        };
        // Hold one dispatch immediately before admission and leave another unpolled.
        let stale = tokio::spawn(dispatch());
        let unpolled = dispatch();
        tokio::time::timeout(StdDuration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();

        // Cancel then explicitly resume: a fresh lifecycle starts while the old dispatch
        // is still stalled before admission.
        runtime.cancel_job(&job_id);
        runtime.resume_job(&job_id);
        hook.release.notify_one();

        let result = tokio::time::timeout(StdDuration::from_secs(10), stale)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result
                .expect_err("stale dispatch must not transfer")
                .message(),
            STALE_TRANSFER_DISPATCH
        );
        assert_eq!(
            unpolled
                .await
                .expect_err("queued dispatch must not transfer")
                .message(),
            STALE_TRANSFER_DISPATCH
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"source content");
        assert!(!destination.exists());

        dispatch()
            .await
            .expect("post-resume dispatch must transfer");
        assert_eq!(std::fs::read(&destination).unwrap(), b"source content");
        runtime.cancel_job(&job_id);
    }

    #[tokio::test]
    async fn resume_after_preflight_admission_makes_stale_cancellation_benign() {
        let runtime = app_runtime();
        let job_id = JobId(9_100_040);
        runtime
            .job_manager
            .track_job(job_id.clone(), JobState::Running, 1, None, vec![]);
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::write(&source, b"source content").unwrap();

        let hook = AdmissionHook::install(&job_id, AdmissionStage::Preflight);
        let pending = tokio::spawn(Arc::clone(runtime).apply_pending_item(
            job_id.clone(),
            0,
            PendingFileItem {
                source: local_ref(&source),
                destination_path: destination.to_str().unwrap().to_owned(),
                installed_path: None,
                undoable: true,
            },
            true,
            ConflictResolution::Overwrite,
            0,
        ));
        tokio::time::timeout(StdDuration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        assert!(has_active_transfer(&job_id));
        runtime.cancel_job(&job_id);
        runtime.resume_job(&job_id);
        hook.release.notify_one();

        let error = tokio::time::timeout(StdDuration::from_secs(10), pending)
            .await
            .unwrap()
            .unwrap()
            .expect_err("stale admission must not transfer");
        assert_eq!(error.message(), STALE_TRANSFER_DISPATCH);
        assert_eq!(std::fs::read(&source).unwrap(), b"source content");
        assert!(!destination.exists());
        runtime.cancel_job(&job_id);
    }

    #[tokio::test]
    async fn transfer_admission_scopes_controls_and_preserves_pause_and_fresh_resume() {
        let runtime = app_runtime();
        let job_id = JobId(9_100_020);
        runtime
            .job_manager
            .track_job(job_id.clone(), JobState::Running, 1, None, vec![]);
        let first = runtime.admit_transfer(&job_id).unwrap();
        runtime.pause_job(&job_id);
        assert!(*first.control().pause_receiver().borrow());
        drop(first);
        assert!(!has_active_transfer(&job_id));
        let paused = runtime.admit_transfer(&job_id).unwrap();
        assert!(*paused.control().pause_receiver().borrow());
        let concurrent = runtime.admit_transfer(&job_id).unwrap();
        drop(paused);
        assert!(has_active_transfer(&job_id));
        runtime.resume_job(&job_id);
        assert!(!*concurrent.control().pause_receiver().borrow());
        runtime.cancel_job(&job_id);
        assert!(concurrent.control().cancel_token().is_cancelled());
        runtime.pause_job(&job_id);
        runtime.fail_job(&job_id);
        runtime.complete_job(&job_id);
        assert_eq!(
            runtime.job_manager.get_state(&job_id),
            Some(JobState::Canceled)
        );
        for state in [JobState::Running, JobState::WaitingRemote] {
            assert_eq!(
                runtime
                    .set_transfer_job_state(&job_id, concurrent.control(), state)
                    .unwrap_err(),
                "transfer canceled"
            );
        }
        runtime.resume_job(&job_id);
        let fresh = runtime.admit_transfer(&job_id).unwrap();
        assert!(!fresh.control().cancel_token().is_cancelled());
        assert!(concurrent.control().cancel_token().is_cancelled());
        drop(concurrent);
        assert!(has_active_transfer(&job_id));
        drop(fresh);
        assert!(!has_active_transfer(&job_id));

        let temp = tempfile::tempdir().unwrap();
        let error = Arc::clone(runtime)
            .apply_pending_item(
                job_id.clone(),
                0,
                PendingFileItem {
                    source: local_ref(temp.path()),
                    destination_path: temp.path().join("child").to_str().unwrap().to_owned(),
                    installed_path: None,
                    undoable: true,
                },
                true,
                ConflictResolution::Overwrite,
                0,
            )
            .await
            .unwrap_err();
        assert!(error
            .message()
            .contains("into itself or one of its descendants"));
        assert!(!has_active_transfer(&job_id));
    }

    fn local_ref(path: &Path) -> ObjectRef {
        ObjectRef {
            local_path_is_lossy: false,
            key: ObjectKey {
                backend_id: BackendId("local".to_string()),
                stable_id: StableId(path.to_string_lossy().into_owned()),
            },
            parent_stable_id: None,
            display_path: path.to_string_lossy().into_owned(),
        }
    }

    async fn assert_rejected_without_writes(source: &Path, destination: &Path) {
        // Neither conflict resolution nor moving may bypass the containment invariant.
        for is_move in [false, true] {
            for resolution in [
                ConflictResolution::Overwrite,
                ConflictResolution::Rename,
                ConflictResolution::Skip,
            ] {
                let error = Arc::clone(app_runtime())
                    .apply_pending_item(
                        JobId(9_000_002),
                        0,
                        PendingFileItem {
                            source: local_ref(source),
                            destination_path: destination.to_string_lossy().into_owned(),
                            installed_path: None,
                            undoable: true,
                        },
                        is_move,
                        resolution,
                        0,
                    )
                    .await
                    .expect_err("recursive directory transfer must be rejected");
                assert!(
                    error
                        .message()
                        .contains("into itself or one of its descendants"),
                    "{error}"
                );
                assert_eq!(std::fs::read_dir(source).unwrap().count(), 0);
            }
        }
    }

    #[tokio::test]
    async fn directory_paste_into_itself_is_rejected_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        assert_rejected_without_writes(&source, &source).await;
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn directory_paste_into_descendant_is_rejected_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        // Multiple absent components exercise nearest-existing-ancestor resolution.
        assert_rejected_without_writes(&source, &source.join("missing/copy")).await;
        assert!(!source.join("missing").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn directory_paste_via_symlink_alias_is_rejected_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let alias = temp.path().join("alias");
        std::fs::create_dir(&source).unwrap();
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        assert_rejected_without_writes(&source, &alias).await;
        assert_rejected_without_writes(&source, &alias.join("missing/copy")).await;
        assert!(!source.join("missing").exists());
    }

    #[tokio::test]
    async fn directory_copy_to_sibling_succeeds() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        // A string prefix is not necessarily a path-component prefix.
        let destination = temp.path().join("source-copy");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("file.txt"), b"content").unwrap();
        Arc::clone(app_runtime())
            .apply_pending_item(
                JobId(9_000_003),
                0,
                PendingFileItem {
                    source: local_ref(&source),
                    destination_path: destination.to_string_lossy().into_owned(),
                    installed_path: None,
                    undoable: true,
                },
                false,
                ConflictResolution::Fail,
                0,
            )
            .await
            .expect("sibling copy succeeds");
        assert_eq!(
            std::fs::read(destination.join("file.txt")).unwrap(),
            b"content"
        );
        assert!(source.join("file.txt").exists());
    }

    #[tokio::test]
    async fn directory_source_batch_declines_before_validation_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let sibling = temp.path().join("sibling");
        std::fs::create_dir(&source).unwrap();
        let items = [sibling.clone(), source.join("copy")].map(|destination| PendingFileItem {
            source: local_ref(&source),
            destination_path: destination.to_string_lossy().into_owned(),
            installed_path: None,
            undoable: true,
        });
        // The recursive destination would fail validation, but an aggregate batch cannot
        // own directory creation: it must be declined before validation or any transfer.
        let outcome = app_runtime()
            .try_rsync_batch(&items, false, &ConflictResolution::Overwrite)
            .await
            .expect("directory batch declined");
        assert!(outcome.is_none());
        assert!(!sibling.exists());
        assert_eq!(std::fs::read_dir(&source).unwrap().count(), 0);
    }

    fn runtime_with_rsync_mapping(root: &Path) -> Arc<AppRuntime> {
        let runtime = AppRuntime::initialize();
        runtime.set_remote_transport_mappings(vec![
            crate::remote_transport::RemoteTransportMapping {
                id: "test-rsync".into(),
                local_mount_path: root.to_string_lossy().into_owned(),
                host: "nas.local".into(),
                ssh_port: 22,
                ssh_username: "admin".into(),
                remote_base_path: "/volume1/data".into(),
                transport: crate::remote_transport::RemoteTransportKind::RsyncSsh,
                auth: crate::remote_transport::SshAuthKind::Agent,
                ssh_identity_file: String::new(),
                ssh_password: String::new(),
                enabled: true,
                auto_detected: false,
                notes: String::new(),
            },
        ]);
        runtime
    }

    #[tokio::test]
    async fn file_source_batch_still_reaches_preflight_validation() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("photo.jpg");
        std::fs::write(&source, b"jpeg").unwrap();
        let mut source_ref = local_ref(&source);
        source_ref.local_path_is_lossy = true;
        let destination = temp.path().join("out").join("photo.jpg");
        let items = vec![PendingFileItem {
            source: source_ref,
            destination_path: destination.to_string_lossy().into_owned(),
            installed_path: None,
            undoable: true,
        }];

        let error = runtime_with_rsync_mapping(destination.parent().expect("destination parent"))
            .try_rsync_batch(&items, false, &ConflictResolution::Overwrite)
            .await
            .expect_err("regular files keep the batch fast path");
        assert!(error.contains("losslessly"), "{error}");
        assert!(!destination.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn remote_directory_destination_uses_backend_and_path_components() {
        let runtime = app_runtime();
        // Use the runtime's virtual root spelling instead of requiring a live remote adapter.
        let root = virtual_root_path(&BackendId("s3:test".into()));
        let mut source = local_ref(&root.join("folder"));
        source.key.backend_id = BackendId("s3:test".into());
        for destination in [root.join("folder"), root.join("folder/child")] {
            assert!(runtime
                .ensure_directory_destination_outside_source(&source, &destination)
                .await
                .is_err());
        }
        for destination in [
            root.join("folder-copy"),
            virtual_root_path(&BackendId("s3:other".into())).join("folder/child"),
        ] {
            runtime
                .ensure_directory_destination_outside_source(&source, &destination)
                .await
                .unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn copying_a_folder_keeps_symlinks_and_does_not_follow_a_link_to_an_ancestor() {
        let source_root = tempfile::tempdir().expect("source tempdir");
        let destination_root = tempfile::tempdir().expect("destination tempdir");
        let prefix = source_root.path().join("prefix");
        std::fs::create_dir_all(prefix.join("drive_c")).expect("create tree");
        std::fs::write(prefix.join("drive_c/app.exe"), b"binary").expect("write file");
        std::fs::create_dir(prefix.join("dosdevices")).expect("create dosdevices");
        // Wine maps a drive letter to the file system root; following it copies the whole disk.
        std::os::unix::fs::symlink("/", prefix.join("dosdevices/z:")).expect("link to root");
        std::os::unix::fs::symlink("../drive_c", prefix.join("dosdevices/c:"))
            .expect("relative link");

        let destination = destination_root.path().join("prefix");
        tokio::time::timeout(
            Duration::from_secs(20),
            Arc::clone(app_runtime()).apply_pending_item(
                JobId(9_000_001),
                0,
                PendingFileItem {
                    source: local_ref(&prefix),
                    destination_path: destination.to_string_lossy().into_owned(),
                    installed_path: None,
                    undoable: true,
                },
                false,
                ConflictResolution::Fail,
                0,
            ),
        )
        .await
        .expect("copy must not recurse through the root link")
        .expect("copy succeeds");

        assert_eq!(
            std::fs::read(destination.join("drive_c/app.exe")).expect("copied file"),
            b"binary"
        );
        assert_eq!(
            std::fs::read_link(destination.join("dosdevices/z:")).expect("root link"),
            Path::new("/")
        );
        assert_eq!(
            std::fs::read_link(destination.join("dosdevices/c:")).expect("relative link"),
            Path::new("../drive_c")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dangling_destination_symlink_honors_copy_conflict_policies() {
        let runtime = app_runtime();
        for (index, resolution) in [
            ConflictResolution::Fail,
            ConflictResolution::Skip,
            ConflictResolution::Rename,
            ConflictResolution::Overwrite,
        ]
        .into_iter()
        .enumerate()
        {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            let destination = temp.path().join("copy.txt");
            let first_candidate = temp.path().join("copy (1).txt");
            let renamed = temp.path().join("copy (2).txt");
            std::fs::write(temp.path().join("payload"), b"source content").unwrap();
            std::os::unix::fs::symlink("payload", &source).unwrap();
            std::os::unix::fs::symlink("old-missing-target", &destination).unwrap();
            std::os::unix::fs::symlink("other-missing-target", &first_candidate).unwrap();

            // Read metadata still follows links and may cache a missing target. Neither
            // that negative cache nor a dangling Rename candidate may hide occupied entries.
            assert!(runtime
                .stat_path(destination.to_str().unwrap())
                .await
                .unwrap()
                .is_none());
            assert!(runtime
                .destination_entry_exists(&destination)
                .await
                .unwrap());
            assert!(!runtime.destination_entry_exists(&renamed).await.unwrap());
            let sync = resolve_destination_path(&destination, &resolution);
            let asynchronous = runtime
                .resolve_destination_path_async(&destination, &resolution)
                .await;
            if matches!(resolution, ConflictResolution::Fail) {
                assert!(sync.unwrap_err().contains("destination already exists"));
                assert!(asynchronous
                    .unwrap_err()
                    .contains("destination already exists"));
            } else {
                let expected = if matches!(resolution, ConflictResolution::Rename) {
                    &renamed
                } else {
                    &destination
                };
                assert_eq!(sync.unwrap(), *expected);
                assert_eq!(asynchronous.unwrap(), *expected);
            }

            let result = Arc::clone(runtime)
                .apply_pending_item(
                    JobId(9_000_010 + index as u64),
                    0,
                    PendingFileItem {
                        source: local_ref(&source),
                        destination_path: destination.to_string_lossy().into_owned(),
                        installed_path: None,
                        undoable: true,
                    },
                    false,
                    resolution.clone(),
                    0,
                )
                .await;
            if matches!(resolution, ConflictResolution::Fail) {
                assert!(result
                    .unwrap_err()
                    .message()
                    .contains("destination already exists"));
            } else {
                result.unwrap();
            }
            let expected_target = if matches!(resolution, ConflictResolution::Overwrite) {
                Path::new("payload")
            } else {
                Path::new("old-missing-target")
            };
            assert_eq!(std::fs::read_link(&destination).unwrap(), expected_target);
            assert_eq!(
                std::fs::read_link(&first_candidate).unwrap(),
                Path::new("other-missing-target")
            );
            if matches!(resolution, ConflictResolution::Rename) {
                assert_eq!(std::fs::read_link(&renamed).unwrap(), Path::new("payload"));
            } else {
                assert!(std::fs::symlink_metadata(&renamed).is_err());
            }
            assert_eq!(std::fs::read_link(&source).unwrap(), Path::new("payload"));
            assert_eq!(std::fs::read(&source).unwrap(), b"source content");
            assert!(!temp.path().join("old-missing-target").exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_recreation_rejects_conflicts_appearing_after_preflight() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::write(temp.path().join("payload"), b"content").unwrap();
        std::os::unix::fs::symlink("payload", &source).unwrap();
        std::os::unix::fs::symlink("missing", &destination).unwrap();
        for resolution in [
            ConflictResolution::Fail,
            ConflictResolution::Skip,
            ConflictResolution::Rename,
        ] {
            // Invoke the mutation branch directly, as if the entry appeared after resolution.
            assert!(app_runtime()
                .transfer_local_symlink(&local_ref(&source), &destination, false, &resolution)
                .await
                .is_err());
            assert_eq!(
                std::fs::read_link(&destination).unwrap(),
                Path::new("missing")
            );
            assert!(!temp.path().join("missing").exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn destination_probe_errors_are_not_absence() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let parent = temp.path().join("loop");
        let destination = parent.join("destination");
        std::os::unix::fs::symlink("missing", &source).unwrap();
        // Unlike chmod-based denial, a looping parent reliably errors even as root.
        std::os::unix::fs::symlink("loop", &parent).unwrap();
        let probe_error = std::fs::symlink_metadata(&destination).unwrap_err();
        assert_ne!(probe_error.kind(), std::io::ErrorKind::NotFound);
        assert!(app_runtime()
            .destination_entry_exists(&destination)
            .await
            .is_err());
        for resolution in [
            ConflictResolution::Fail,
            ConflictResolution::Skip,
            ConflictResolution::Rename,
            ConflictResolution::Overwrite,
        ] {
            assert!(resolve_destination_path(&destination, &resolution).is_err());
            assert!(app_runtime()
                .resolve_destination_path_async(&destination, &resolution)
                .await
                .is_err());
            assert!(Arc::clone(app_runtime())
                .apply_pending_item(
                    JobId(9_000_020),
                    0,
                    PendingFileItem {
                        source: local_ref(&source),
                        destination_path: destination.to_string_lossy().into_owned(),
                        installed_path: None,
                        undoable: true,
                    },
                    false,
                    resolution,
                    0,
                )
                .await
                .is_err());
        }
        assert_eq!(std::fs::read_link(&parent).unwrap(), Path::new("loop"));
        assert_eq!(std::fs::read_link(&source).unwrap(), Path::new("missing"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_creation_failure_preserves_existing_file_or_dangling_link() {
        for dangling in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let destination = temp.path().join("destination");
            if dangling {
                std::os::unix::fs::symlink("old-missing-target", &destination).unwrap();
            } else {
                std::fs::write(&destination, b"original bytes").unwrap();
            }
            let error = recreate_symlink_with(
                &destination,
                true,
                |staged| {
                    assert_ne!(staged, destination);
                    assert_eq!(staged.parent(), destination.parent());
                    // A missing parent forces a real creation error even when run as root.
                    std::os::unix::fs::symlink("new-target", staged.join("missing/link"))
                },
                |_, _| panic!("creation failed: must not commit"),
            )
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            if dangling {
                assert_eq!(
                    std::fs::read_link(&destination).unwrap(),
                    Path::new("old-missing-target")
                );
            } else {
                assert_eq!(std::fs::read(&destination).unwrap(), b"original bytes");
            }
            assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_commit_failure_preserves_destination_and_removes_staging() {
        for dangling in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let destination = temp.path().join("destination");
            if dangling {
                std::os::unix::fs::symlink("old-missing-target", &destination).unwrap();
            } else {
                std::fs::write(&destination, b"original bytes").unwrap();
            }
            let error = recreate_symlink_with(
                &destination,
                true,
                |staged| std::os::unix::fs::symlink("new-target", staged),
                |staged, dst| {
                    assert_eq!(dst, destination);
                    assert_eq!(std::fs::read_link(staged).unwrap(), Path::new("new-target"));
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected replace failure (e.g. Windows sharing lock)",
                    ))
                },
            )
            .unwrap_err();
            // Occupancy must stop the caller from falling back through the original entry.
            assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
            if dangling {
                assert_eq!(
                    std::fs::read_link(&destination).unwrap(),
                    Path::new("old-missing-target")
                );
            } else {
                assert_eq!(std::fs::read(&destination).unwrap(), b"original bytes");
            }
            assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_commit_refuses_a_racing_directory_and_removes_staging() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("destination");
        let error = recreate_symlink_with(
            &destination,
            true,
            |staged| {
                std::fs::create_dir(&destination).unwrap();
                std::fs::write(destination.join("keep"), b"original bytes").unwrap();
                std::os::unix::fs::symlink("new-target", staged)
            },
            |staged, dst| std::fs::rename(staged, dst),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(destination.join("keep")).unwrap(),
            b"original bytes"
        );
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn recreating_a_symlink_replaces_a_file_but_never_a_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let link = temp.path().join("link");
        std::os::unix::fs::symlink("target.txt", &link).expect("create link");

        let over_file = temp.path().join("over-file");
        std::fs::write(&over_file, b"old").expect("write file");
        recreate_symlink(&link, &over_file, true).expect("replace file");
        assert_eq!(
            std::fs::read_link(&over_file).expect("read link"),
            Path::new("target.txt")
        );

        let over_dir = temp.path().join("over-dir");
        std::fs::create_dir(&over_dir).expect("create dir");
        assert!(recreate_symlink(&link, &over_dir, true).is_err());
        assert!(over_dir.is_dir());
    }
}
