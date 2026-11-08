use super::*;
use crate::i18n::{tr, tr_with};
use crate::state::PendingUndoAction;

pub fn start_pending_file_operation(
    state: &mut AppState,
    items: Vec<PendingFileItem>,
    is_move: bool,
) -> Task<Message> {
    start_pending_file_operation_with_undo_action(
        state,
        items,
        is_move,
        Some(PendingUndoAction::RecordTransfer),
    )
}

pub fn start_pending_file_operation_with_undo_action(
    state: &mut AppState,
    items: Vec<PendingFileItem>,
    is_move: bool,
    undo_action: Option<PendingUndoAction>,
) -> Task<Message> {
    if items.is_empty() {
        return Task::none();
    }

    let description = if is_move {
        tr_with(
            "file-operation-move-items",
            &[("count", items.len().into())],
        )
    } else {
        tr_with(
            "file-operation-copy-items",
            &[("count", items.len().into())],
        )
    };

    let job_id = next_job_id(state);
    state.jobs.push(JobDisplayInfo {
        id: job_id.clone(),
        description: description.clone(),
        state: JobState::Running,
        progress: iris_core_domain::types::progress::ProgressInfo {
            bytes_completed: 0,
            bytes_total: None,
            files_completed: 0,
            files_total: Some(items.len() as u64),
        },
        speed_samples: std::collections::VecDeque::new(),
        speed_bps: None,
    });

    let max_in_flight = max_concurrent_transfers_for_items(&items);
    let persisted_items = items.clone();
    state.pending_operation = Some(PendingFileOperation::new(
        job_id.clone(),
        items,
        is_move,
        undo_action,
        max_in_flight,
    ));

    // Persisting walks the sources to record their sizes, which can take long for big folders
    // or network shares; do it off the UI thread and start the transfers when it is stored.
    Task::perform(
        async move {
            let persisted_job_id = job_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                app_runtime().persist_pending_job(
                    &persisted_job_id,
                    &description,
                    &persisted_items,
                    is_move,
                )
            })
            .await;
            if let Ok(Err(error)) | Err(error) = result.map_err(|error| error.to_string()) {
                tracing::warn!(
                    "failed to persist transfer job; it cannot resume after restart: {error}"
                );
            }
            job_id
        },
        |job_id| Message::Job(JobMessage::Persisted(job_id)),
    )
}

/// Files transferred concurrently, so one file's I/O wait overlaps another's reads and writes.
const DEFAULT_MAX_CONCURRENT_TRANSFERS: usize = 4;
/// SMB transfers run one at a time on macOS.
const MAX_CONCURRENT_SMB_TRANSFERS: usize = 1;

pub(crate) fn max_concurrent_transfers_for_items(items: &[PendingFileItem]) -> usize {
    if cfg!(target_os = "macos") {
        let runtime = app_runtime();
        let uses_smb = items.iter().any(|item| {
            let src_backend = runtime.backend_id_for_path(Path::new(&item.source.display_path));
            let dst_backend = runtime.backend_id_for_path(Path::new(&item.destination_path));
            src_backend.0.starts_with("smb:") || dst_backend.0.starts_with("smb:")
        });
        if uses_smb {
            return MAX_CONCURRENT_SMB_TRANSFERS;
        }
    }

    DEFAULT_MAX_CONCURRENT_TRANSFERS
}

pub fn process_pending_operation(state: &mut AppState) -> Task<Message> {
    let Some(pending) = &state.pending_operation else {
        return Task::none();
    };
    let job_state = state
        .jobs
        .iter()
        .find(|job| job.id == pending.job_id)
        .map(|job| job.state.clone());
    // Paused and canceled jobs start nothing; a failed one waits for Retry, Skip or Abort.
    if !pending.persisted
        || !pending.failed.is_empty()
        || matches!(
            job_state,
            Some(JobState::Paused | JobState::Canceled | JobState::Failed)
        )
    {
        return Task::none();
    }

    if pending.all_started() {
        return if pending.in_flight == 0 {
            finish_pending_operation(state)
        } else {
            Task::none()
        };
    }

    // No conflict policy yet: check conflicts one file at a time.
    if pending.remember_resolution.is_none() {
        if pending.in_flight > 0 {
            return Task::none();
        }
        let Some(index) = pending.next_index() else {
            return Task::none();
        };
        let item = pending.items[index].clone();
        let job_id = pending.job_id.clone();
        if let Some(pending) = &mut state.pending_operation {
            pending.in_flight = 1;
        }
        return Task::perform(
            async move {
                app_runtime()
                    .conflict_metadata(&item.source, &item.destination_path)
                    .await
            },
            move |result| Message::ConflictCheckCompleted(job_id.clone(), index, result),
        );
    }

    let resolution = pending
        .remember_resolution
        .clone()
        .unwrap_or(ConflictResolution::Overwrite);
    let base_bytes = state
        .jobs
        .iter()
        .find(|job| job.id == pending.job_id)
        .map(|job| job.progress.bytes_completed)
        .unwrap_or(0);

    // rsync fast path for all remaining files: only for a fresh start (nothing in flight or to
    // retry), when both ends are local-mount paths, rsync is available, and the policy is not
    // Rename (rsync cannot rename).
    let remaining = pending.items.len() - pending.current_index;
    if remaining >= 2
        && pending.in_flight == 0
        && pending.retry.is_empty()
        && !matches!(resolution, ConflictResolution::Rename)
        && !pending.rsync_failed
    {
        let items = pending.items[pending.current_index..].to_vec();
        let job_id = pending.job_id.clone();
        let is_move = pending.is_move;
        let item_count = items.len();
        // Advance current_index past every file; the single rsync task is the one in flight.
        if let Some(pending) = &mut state.pending_operation {
            pending.current_index = pending.items.len();
            pending.in_flight = 1;
        }
        return Task::perform(
            async move {
                let runtime = app_runtime();
                match runtime.try_rsync_batch(&items, is_move, &resolution).await {
                    Ok(Some(total_bytes)) => Ok(PendingItemResult {
                        bytes_completed: total_bytes,
                        files_completed: item_count as u64,
                    }),
                    // Sentinel meaning "fall back to the normal path".
                    _ => Ok(PendingItemResult {
                        bytes_completed: 0,
                        files_completed: 0,
                    }),
                }
            },
            move |result| Message::RsyncBatchCompleted(job_id.clone(), item_count, result),
        );
    }

    // Keep up to `max_in_flight` transfers running: each completion starts the next file, so a
    // slow file does not hold back the others.
    let Some(pending) = &mut state.pending_operation else {
        return Task::none();
    };
    let mut tasks = Vec::new();
    while pending.in_flight < pending.max_in_flight {
        let Some(index) = pending.take_next_index() else {
            break;
        };
        tasks.push(spawn_apply_one_item(
            &pending.job_id,
            index,
            &pending.items[index],
            pending.is_move,
            resolution.clone(),
            base_bytes,
        ));
        pending.in_flight += 1;
    }
    Task::batch(tasks)
}

fn finish_pending_operation(state: &mut AppState) -> Task<Message> {
    let Some(pending) = state.pending_operation.take() else {
        return Task::none();
    };
    let mut completion_tasks = vec![Task::done(Message::Refresh)];
    match pending.undo_action {
        Some(PendingUndoAction::RecordTransfer) => {
            let undo_kind = if pending.is_move {
                UndoOpKind::Move
            } else {
                UndoOpKind::Copy
            };
            let undo_items = pending
                .items
                .iter()
                .map(|item| UndoItem {
                    source_path: PathBuf::from(&item.source.display_path),
                    destination_path: PathBuf::from(&item.destination_path),
                    size: None,
                    mtime: None,
                    trashed_path: None,
                    trash_id: None,
                })
                .collect();
            completion_tasks.push(record_undo_op_task(undo_kind, undo_items));
        }
        Some(PendingUndoAction::RecordOp(op)) => {
            state.undo_journal.push_undo_from_redo(op);
        }
        Some(PendingUndoAction::PushRedo(op)) => {
            state.undo_journal.push_redo(op);
        }
        None => {}
    }
    if let Some(job) = state.jobs.iter_mut().find(|job| job.id == pending.job_id) {
        job.state = JobState::Completed;
        reset_job_speed(job);
    }
    app_runtime().complete_job(&pending.job_id);
    state.add_notification(
        NotificationSeverity::Info,
        tr("file-operation-completed-title"),
        tr("file-operation-completed-body"),
        vec![NotificationAction::Dismiss],
    );
    Task::batch(completion_tasks)
}

pub fn apply_conflict_resolution(
    state: &mut AppState,
    resolution: ConflictResolution,
) -> Task<Message> {
    let base_bytes = state
        .pending_operation
        .as_ref()
        .and_then(|pending| state.jobs.iter().find(|job| job.id == pending.job_id))
        .map(|job| job.progress.bytes_completed)
        .unwrap_or(0);
    let Some(pending) = &mut state.pending_operation else {
        return Task::none();
    };
    let Some(index) = pending.take_next_index() else {
        return Task::none();
    };
    pending.in_flight = 1;
    spawn_apply_one_item(
        &pending.job_id,
        index,
        &pending.items[index],
        pending.is_move,
        resolution,
        base_bytes,
    )
}

/// Retry: the failed items start again (before new ones); Skip: they are recorded as skipped.
pub fn resolve_failed_items(state: &mut AppState, job_id: &JobId, retry: bool) -> Task<Message> {
    dismiss_job_decisions(state, job_id);
    let Some(pending) = state
        .pending_operation
        .as_mut()
        .filter(|pending| pending.job_id == *job_id)
    else {
        return Task::none();
    };
    let failed = std::mem::take(&mut pending.failed);
    if retry {
        // `retry` is used as a stack; keep the original order.
        pending.retry.extend(failed.into_iter().rev());
    } else {
        for index in failed {
            app_runtime().skip_task(job_id, index);
        }
    }
    if let Some(job) = state.jobs.iter_mut().find(|job| job.id == *job_id) {
        job.state = JobState::Running;
    }
    app_runtime().resume_job(job_id);
    process_pending_operation(state)
}

/// Removes the Retry / Skip / Abort notification of a job once one of them was chosen.
pub fn dismiss_job_decisions(state: &mut AppState, job_id: &JobId) {
    state.notifications.retain(|notification| {
        !notification
            .actions
            .iter()
            .any(|action| action.job_id() == Some(job_id))
    });
}

/// Cancels a job in the runtime and marks it canceled in the UI state.
pub fn cancel_job_state(state: &mut AppState, job_id: &JobId) {
    dismiss_job_decisions(state, job_id);
    app_runtime().cancel_job(job_id);
    if let Some(job) = state.jobs.iter_mut().find(|job| job.id == *job_id) {
        job.state = JobState::Canceled;
    }
    if let Some(pending) = &state.pending_operation {
        if pending.job_id == *job_id {
            state.pending_operation = None;
            state.active_dialog = None;
        }
    }
}

fn spawn_apply_one_item(
    job_id: &JobId,
    task_index: usize,
    item: &PendingFileItem,
    is_move: bool,
    resolution: ConflictResolution,
    base_bytes_completed: u64,
) -> Task<Message> {
    let job_id = job_id.clone();
    let callback_job_id = job_id.clone();
    let runtime = Arc::clone(app_runtime());
    let item = item.clone();

    Task::perform(
        async move {
            runtime
                .apply_pending_item(
                    job_id.clone(),
                    task_index,
                    item,
                    is_move,
                    resolution,
                    base_bytes_completed,
                )
                .await
                .map(|outcome| PendingItemResult {
                    bytes_completed: outcome.bytes_completed,
                    files_completed: 1,
                })
        },
        move |result| Message::PendingItemCompleted(callback_job_id, task_index, result),
    )
}

pub fn fail_pending_job(state: &mut AppState, job_id: &JobId, reason: &str) {
    app_runtime().fail_job(job_id);
    if let Some(job) = state.jobs.iter_mut().find(|j| j.id == *job_id) {
        job.state = JobState::Failed;
        reset_job_speed(job);
    }
    state.add_notification(
        NotificationSeverity::Error,
        tr("file-operation-failed-title"),
        reason.to_string(),
        vec![
            NotificationAction::Retry(job_id.clone()),
            NotificationAction::SkipAndContinue(job_id.clone()),
            NotificationAction::AbortAll(job_id.clone()),
        ],
    );
}
