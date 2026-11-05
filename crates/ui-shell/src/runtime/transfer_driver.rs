use super::*;

struct TransferExecutionContext<'a> {
    job_id: &'a JobId,
    task_index: usize,
    record_chunk_state: bool,
    progress_tx: &'a watch::Sender<ProgressInfo>,
    control: &'a TransferControl,
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

impl AppRuntime {
    pub async fn apply_pending_item(
        self: Arc<Self>,
        job_id: JobId,
        task_index: usize,
        item: PendingFileItem,
        is_move: bool,
        resolution: ConflictResolution,
        base_bytes_completed: u64,
    ) -> Result<TransferItemOutcome, String> {
        let task_id = persisted_task_id(&job_id, task_index);
        let control = TransferControl::new();
        self.active_transfers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(job_id.0, control.clone());

        let (progress_tx, mut progress_rx) = watch::channel(ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: Some(1),
        });

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
            control: &control,
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

        self.active_transfers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&job_id.0);

        match transfer_result {
            Ok(outcome) => {
                self.update_task_state(&job_id, task_index, TaskState::Completed, None);
                Ok(outcome)
            }
            Err(error) if error == "transfer canceled" => {
                self.update_task_state(&job_id, task_index, TaskState::Failed, Some(&error));
                self.persist_job_state(&job_id, JobState::Canceled);
                Err(error)
            }
            Err(error) => {
                self.update_task_state(&job_id, task_index, TaskState::Failed, Some(&error));
                Err(error)
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
            let TransferPathRequest {
                source,
                destination,
                is_move,
                resolution,
            } = request;
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

                self.persist_job_state(job_id, JobState::Running);
                self.job_manager.update_state(job_id, JobState::Running);
                self.update_task_state(job_id, task_index, TaskState::Running, None);
            }

            if matches!(resolution, ConflictResolution::Skip) && self.path_exists(dst_path).await? {
                self.update_task_state(job_id, task_index, TaskState::Skipped, None);
                return Ok(TransferItemOutcome {
                    skipped: true,
                    bytes_completed: 0,
                });
            }

            let final_dst = self
                .resolve_destination_path_async(dst_path, &resolution)
                .await?;

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
                    .await;
            }

            self.transfer_file(context, source, &final_dst, is_move, resolution)
                .await
                .map(|result| TransferItemOutcome {
                    skipped: false,
                    bytes_completed: result.bytes_transferred,
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
            };
            let TransferDirectoryRequest {
                source,
                destination,
                is_move,
                resolution,
            } = request;
            wait_if_paused_or_canceled(control).await?;
            let src_path = Path::new(&source.display_path);
            let src_resolved = self.resolve_backend(src_path);
            let dst_resolved = self.resolve_backend(destination);
            let same_backend =
                self.same_backend(src_path, &src_resolved, destination, &dst_resolved);
            let high_risk_path = src_resolved.kind != VolumeKind::Local
                || dst_resolved.kind != VolumeKind::Local
                || src_path.parent() != destination.parent();

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
                src_resolved
                    .adapter
                    .move_object(
                        &src_ref,
                        &dst_ref,
                        MoveOptions {
                            on_conflict: resolution.clone(),
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                self.rebind_cached_display_path_prefix(
                    &src_resolved.backend_id,
                    &source.display_path,
                    &destination.to_string_lossy(),
                );
                return Ok(TransferItemOutcome {
                    skipped: false,
                    bytes_completed: 0,
                });
            }

            self.ensure_directory_exists(&dst_resolved, destination)
                .await?;

            let mut total_bytes = 0u64;
            for child in self.list_directory_children(source, &src_resolved).await? {
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
            }

            if is_move {
                let dst_ref = self.object_ref_for_path(destination, &dst_resolved)?;
                if src_resolved.kind == VolumeKind::Local
                    && dst_resolved.kind == VolumeKind::Local
                    && !self.is_virtual_display_path(&source.display_path)
                    && !self.is_virtual_display_path(destination.to_string_lossy().as_ref())
                {
                    verify_directory_mirror(src_path, destination, high_risk_path)?;
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
                self.delete_object(source, false).await?;
            }

            Ok(TransferItemOutcome {
                skipped: false,
                bytes_completed: total_bytes,
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
        let high_risk_path = src_resolved.kind != VolumeKind::Local
            || dst_resolved.kind != VolumeKind::Local
            || src_path.parent() != dst.parent();
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
                    src_resolved
                        .adapter
                        .move_object(
                            &src_ref,
                            &dst_ref,
                            MoveOptions {
                                on_conflict: resolution.clone(),
                            },
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    self.rebind_cached_display_path_prefix(
                        &src_resolved.backend_id,
                        &src.display_path,
                        &dst.to_string_lossy(),
                    );
                    let total_bytes = self
                        .stat_path(dst.to_string_lossy().as_ref())
                        .await?
                        .and_then(|meta| meta.size)
                        .unwrap_or(0);
                    let _ = progress_tx.send(ProgressInfo {
                        bytes_completed: total_bytes,
                        bytes_total: Some(total_bytes),
                        files_completed: 1,
                        files_total: Some(1),
                    });
                    record_transfer_sample(&src_resolved, &dst_resolved, total_bytes, started_at);
                    return Ok(TransferResult {
                        bytes_transferred: total_bytes,
                    });
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
                    )
                    .await?;
                    self.reconcile_cache_after_move(src, &src_resolved, dst, &dst_resolved)
                        .await;
                    let total_bytes = self
                        .stat_path(dst.to_string_lossy().as_ref())
                        .await?
                        .and_then(|meta| meta.size)
                        .unwrap_or(0);
                    let _ = progress_tx.send(ProgressInfo {
                        bytes_completed: total_bytes,
                        bytes_total: Some(total_bytes),
                        files_completed: 1,
                        files_total: Some(1),
                    });
                    record_transfer_sample(&src_resolved, &dst_resolved, total_bytes, started_at);
                    return Ok(TransferResult {
                        bytes_transferred: total_bytes,
                    });
                }
                MoveMethod::ClientRelayThenDelete => {
                    let src_mtime = src_resolved
                        .adapter
                        .stat(&src_ref)
                        .await
                        .ok()
                        .and_then(|meta| meta.mtime);

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
                    .map_err(|error| error.to_string())?;

                    if let Some(mtime) = src_mtime {
                        if can_restore_mtime(dst_resolved.kind) {
                            let _ = iris_backends::common::set_mtime(dst, mtime);
                        }
                    }

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
                let total_bytes = self
                    .stat_path(dst.to_string_lossy().as_ref())
                    .await?
                    .and_then(|meta| meta.size)
                    .unwrap_or(0);
                let _ = progress_tx.send(ProgressInfo {
                    bytes_completed: total_bytes,
                    bytes_total: Some(total_bytes),
                    files_completed: 1,
                    files_total: Some(1),
                });
                record_transfer_sample(&src_resolved, &dst_resolved, total_bytes, started_at);
                Ok(TransferResult {
                    bytes_transferred: total_bytes,
                })
            }
            CopyMethod::ClientRelayCopy => {
                let src_mtime = src_resolved
                    .adapter
                    .stat(&src_ref)
                    .await
                    .ok()
                    .and_then(|meta| meta.mtime);

                let result = client_relay_copy_with_options(
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

                if let Some(mtime) = src_mtime {
                    if can_restore_mtime(dst_resolved.kind) {
                        let _ = iris_backends::common::set_mtime(dst, mtime);
                    }
                }

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
            self.job_manager
                .update_state(job_id, JobState::WaitingRemote);
            self.persist_job_state(job_id, JobState::WaitingRemote);
            sleep(Duration::from_millis(50)).await;
        }

        Ok(())
    }
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
