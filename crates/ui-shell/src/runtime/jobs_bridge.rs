use super::*;

impl AppRuntime {
    pub fn restore_jobs(&self) -> Vec<RestoredJob> {
        self.job_manager
            .restore_jobs(|task| self.persisted_task_is_reachable(task))
            .unwrap_or_default()
            .into_iter()
            .map(|job| RestoredJob {
                description: job.description,
                id: job.id,
                state: job.state,
                files_total: job.total_files,
            })
            .collect()
    }

    pub fn persist_pending_job(
        &self,
        job_id: &JobId,
        description: &str,
        items: &[PendingFileItem],
        is_move: bool,
    ) -> Result<(), String> {
        let created_at = chrono::Utc::now().to_rfc3339();
        let tasks = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let destination_backend =
                    self.backend_id_for_path(Path::new(&item.destination_path));
                PersistedTask {
                    id: persisted_task_id(job_id, index),
                    job_id: job_id.clone(),
                    kind: if is_move {
                        TaskKind::Move
                    } else {
                        TaskKind::Copy
                    },
                    state: TaskState::Pending,
                    src_backend: Some(item.source.key.backend_id.0.clone()),
                    src_stable_id: Some(item.source.key.stable_id.0.clone()),
                    src_display_path: Some(item.source.display_path.clone()),
                    dst_backend: Some(destination_backend.0),
                    dst_stable_id: std::fs::metadata(&item.destination_path)
                        .ok()
                        .map(|_| self.stable_id_for_path(Path::new(&item.destination_path)).0),
                    dst_display_path: Some(item.destination_path.clone()),
                    bytes_total: path_total_bytes(Path::new(&item.source.display_path))
                        .unwrap_or(0),
                    bytes_completed: 0,
                    error_message: None,
                }
            })
            .collect::<Vec<_>>();

        let persisted_job = PersistedJob {
            id: job_id.clone(),
            state: JobState::Queued,
            created_at: created_at.clone(),
            updated_at: created_at,
            description: description.to_string(),
            tasks,
        };

        self.job_store
            .save_job(&persisted_job)
            .map_err(|error| error.to_string())?;
        self.job_manager.track_job(
            job_id.clone(),
            JobState::Queued,
            items.len() as u64,
            None,
            self.backend_types_for_items(items),
        );
        self.job_manager.update_state(job_id, JobState::Queued);
        Ok(())
    }

    pub fn restore_pending_operation(
        &self,
        job_id: &JobId,
    ) -> Result<Option<(Vec<PendingFileItem>, usize, bool)>, String> {
        let job = self
            .job_store
            .load_job(job_id)
            .map_err(|error| error.to_string())?;
        let Some(job) = job else {
            return Ok(None);
        };

        let mut items = Vec::with_capacity(job.tasks.len());
        let mut current_index = job.tasks.len();
        let mut is_move = false;

        for (index, task) in job.tasks.iter().enumerate() {
            if index == 0 {
                is_move = task.kind == TaskKind::Move;
            }

            let source_path = task.src_display_path.clone().unwrap_or_default();
            let destination_path = task.dst_display_path.clone().unwrap_or_default();
            let source_backend = BackendId(
                task.src_backend
                    .clone()
                    .unwrap_or_else(|| "local".to_string()),
            );

            items.push(PendingFileItem {
                source: ObjectRef {
                    key: ObjectKey {
                        backend_id: source_backend,
                        stable_id: StableId(
                            task.src_stable_id
                                .clone()
                                .unwrap_or_else(|| source_path.clone()),
                        ),
                    },
                    parent_stable_id: None,
                    display_path: source_path,
                },
                destination_path,
            });

            if current_index == job.tasks.len()
                && !matches!(task.state, TaskState::Completed | TaskState::Skipped)
            {
                current_index = index;
            }
        }

        if current_index == job.tasks.len() {
            current_index = items.len();
        }

        Ok(Some((items, current_index, is_move)))
    }

    pub fn pause_job(&self, job_id: &JobId) {
        if let Some(control) = self
            .active_transfers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&job_id.0)
            .cloned()
        {
            control.pause();
        }
        self.persist_job_state(job_id, JobState::Paused);
        self.job_manager.pause_job(job_id);
    }

    pub fn resume_job(&self, job_id: &JobId) {
        if let Some(control) = self
            .active_transfers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&job_id.0)
            .cloned()
        {
            control.resume();
        }
        self.persist_job_state(job_id, JobState::Running);
        self.job_manager.resume_job(job_id);
    }

    pub fn cancel_job(&self, job_id: &JobId) {
        if let Some(control) = self
            .active_transfers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&job_id.0)
        {
            control.cancel();
        }
        self.clear_active_job_progress(job_id);
        self.persist_job_state(job_id, JobState::Canceled);
        self.job_manager.cancel_job(job_id);
    }

    pub fn complete_job(&self, job_id: &JobId) {
        self.active_transfers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&job_id.0);
        self.clear_active_job_progress(job_id);
        self.persist_job_state(job_id, JobState::Completed);
        self.job_manager.update_state(job_id, JobState::Completed);
    }

    pub fn fail_job(&self, job_id: &JobId) {
        self.clear_active_job_progress(job_id);
        self.persist_job_state(job_id, JobState::Failed);
        self.job_manager.update_state(job_id, JobState::Failed);
    }

    pub fn skip_task(&self, job_id: &JobId, task_index: usize) {
        self.update_task_state(job_id, task_index, TaskState::Skipped, None);
    }

    pub(super) fn update_active_job_progress(
        &self,
        job_id: &JobId,
        task_id: &TaskId,
        base_bytes_completed: u64,
        task_bytes_completed: u64,
    ) -> u64 {
        let mut guard = self
            .active_transfer_progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tracker = guard.entry(job_id.0).or_default();
        tracker.committed_bytes = tracker.committed_bytes.max(base_bytes_completed);
        tracker
            .task_bytes
            .insert(task_id.clone(), task_bytes_completed);
        tracker.committed_bytes + tracker.task_bytes.values().copied().sum::<u64>()
    }

    pub(super) fn finalize_active_job_progress(
        &self,
        job_id: &JobId,
        task_id: &TaskId,
        base_bytes_completed: u64,
        committed_delta: Option<u64>,
    ) -> u64 {
        let mut guard = self
            .active_transfer_progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tracker = guard.entry(job_id.0).or_default();
        tracker.committed_bytes = tracker.committed_bytes.max(base_bytes_completed);
        tracker.task_bytes.remove(task_id);
        if let Some(delta) = committed_delta {
            tracker.committed_bytes = tracker.committed_bytes.saturating_add(delta);
        }
        tracker.committed_bytes + tracker.task_bytes.values().copied().sum::<u64>()
    }

    pub(super) fn clear_active_job_progress(&self, job_id: &JobId) {
        self.active_transfer_progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&job_id.0);
    }

    pub(super) fn persist_job_state(&self, job_id: &JobId, state: JobState) {
        let updated_at = chrono::Utc::now().to_rfc3339();
        let _ = self.job_store.update_job_state(job_id, &state, &updated_at);
    }

    pub(super) fn update_task_progress(
        &self,
        _job_id: &JobId,
        task_id: &TaskId,
        bytes_completed: u64,
    ) {
        let _ = self
            .job_store
            .update_task_progress(task_id, bytes_completed);
    }

    pub(super) fn update_task_state(
        &self,
        job_id: &JobId,
        task_index: usize,
        state: TaskState,
        error_message: Option<&str>,
    ) {
        let task_id = persisted_task_id(job_id, task_index);
        let _ = self
            .job_store
            .update_task_state(&task_id, &state, error_message);
    }

    pub(super) fn backend_types_for_items(&self, items: &[PendingFileItem]) -> Vec<String> {
        let mut backend_types = Vec::new();
        for item in items {
            for backend_id in [
                item.source.key.backend_id.clone(),
                self.backend_id_for_path(Path::new(&item.destination_path)),
            ] {
                let backend_type = backend_type_from_id(&backend_id);
                if !backend_types.contains(&backend_type) {
                    backend_types.push(backend_type);
                }
            }
        }
        backend_types
    }

    pub(super) fn persisted_task_is_reachable(&self, task: &PersistedTask) -> bool {
        let src_ok = task
            .src_display_path
            .as_deref()
            .map(|path| {
                if self.is_virtual_display_path(path) {
                    self.parse_virtual_display_path(path)
                        .map(|(backend_id, _)| {
                            self.registry
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .get(&backend_id)
                                .is_some()
                        })
                        .unwrap_or(false)
                } else {
                    Path::new(path).exists()
                }
            })
            .unwrap_or(false);
        let dst_ok = task
            .dst_display_path
            .as_deref()
            .and_then(|path| Path::new(path).parent().map(Path::to_path_buf))
            .map(|path| {
                if self.is_virtual_display_path(path.to_string_lossy().as_ref()) {
                    self.parse_virtual_path(&path)
                        .map(|(backend_id, _)| {
                            self.registry
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .get(&backend_id)
                                .is_some()
                        })
                        .unwrap_or(false)
                } else {
                    path.exists()
                }
            })
            .unwrap_or(false);
        src_ok && dst_ok
    }
}
