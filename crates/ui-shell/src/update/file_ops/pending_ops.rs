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

    state.pending_operation = Some(PendingFileOperation {
        items,
        current_index: 0,
        is_move,
        remember_resolution: None,
        job_id,
        in_flight: 0,
        rsync_failed: false,
        undo_action,
    });

    if let Some(pending) = &state.pending_operation {
        let _ = app_runtime().persist_pending_job(
            &pending.job_id,
            &description,
            &pending.items,
            is_move,
        );
    }

    process_pending_operation(state)
}

/// Files transferred concurrently, so one file's I/O wait overlaps another's reads and writes.
const DEFAULT_MAX_CONCURRENT_TRANSFERS: usize = 4;
/// SMB transfers run one at a time on macOS.
const MAX_CONCURRENT_SMB_TRANSFERS: usize = 1;

fn max_concurrent_transfers_for_items(items: &[PendingFileItem]) -> usize {
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
    let pending_snapshot = match &state.pending_operation {
        Some(p) => p.clone(),
        None => return Task::none(),
    };

    if let Some(job) = state.jobs.iter().find(|j| j.id == pending_snapshot.job_id) {
        if job.state == JobState::Paused || job.state == JobState::Canceled {
            return Task::none();
        }
    }

    if pending_snapshot.current_index >= pending_snapshot.items.len() {
        let mut completion_tasks = vec![Task::done(Message::Refresh)];
        match pending_snapshot.undo_action {
            Some(PendingUndoAction::RecordTransfer) => {
                let undo_kind = if pending_snapshot.is_move {
                    UndoOpKind::Move
                } else {
                    UndoOpKind::Copy
                };
                let undo_items = pending_snapshot
                    .items
                    .iter()
                    .map(|item| UndoItem {
                        source_path: PathBuf::from(&item.source.display_path),
                        destination_path: PathBuf::from(&item.destination_path),
                        size: None,
                        mtime: None,
                        trashed_path: None,
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
        if let Some(job) = state
            .jobs
            .iter_mut()
            .find(|j| j.id == pending_snapshot.job_id)
        {
            job.state = JobState::Completed;
            reset_job_speed(job);
        }
        app_runtime().complete_job(&pending_snapshot.job_id);
        state.pending_operation = None;
        state.add_notification(
            NotificationSeverity::Info,
            tr("file-operation-completed-title"),
            tr("file-operation-completed-body"),
            vec![NotificationAction::Dismiss],
        );
        return Task::batch(completion_tasks);
    }

    // No conflict policy yet: check conflicts one file at a time.
    if pending_snapshot.remember_resolution.is_none() {
        let item = pending_snapshot.items[pending_snapshot.current_index].clone();
        let job_id = pending_snapshot.job_id.clone();
        let task_index = pending_snapshot.current_index;
        if let Some(pending) = &mut state.pending_operation {
            pending.in_flight = 1;
        }
        return Task::perform(
            async move {
                app_runtime()
                    .conflict_metadata(&item.source, &item.destination_path)
                    .await
            },
            move |result| Message::ConflictCheckCompleted(job_id.clone(), task_index, result),
        );
    }

    // Conflict policy decided: dispatch up to N files at once.
    let resolution = pending_snapshot
        .remember_resolution
        .clone()
        .unwrap_or(ConflictResolution::Overwrite);

    let base_bytes = state
        .jobs
        .iter()
        .find(|job| job.id == pending_snapshot.job_id)
        .map(|job| job.progress.bytes_completed)
        .unwrap_or(0);

    let remaining = pending_snapshot.items.len() - pending_snapshot.current_index;

    // rsync fast path for the remaining files: only when both ends are local-mount paths, rsync is
    // available, and the policy is not Rename (rsync cannot rename).
    if remaining >= 2
        && !matches!(resolution, ConflictResolution::Rename)
        && !pending_snapshot.rsync_failed
    {
        let items: Vec<PendingFileItem> =
            pending_snapshot.items[pending_snapshot.current_index..].to_vec();
        let job_id = pending_snapshot.job_id.clone();
        let is_move = pending_snapshot.is_move;
        let res = resolution;
        let item_count = items.len();
        // Advance current_index past every file; the single rsync task is the one in flight.
        if let Some(pending) = &mut state.pending_operation {
            pending.current_index = pending.items.len();
            pending.in_flight = 1;
        }
        return Task::perform(
            async move {
                let runtime = app_runtime();
                match runtime.try_rsync_batch(&items, is_move, &res).await {
                    Ok(Some(total_bytes)) => Ok(PendingItemResult {
                        skipped: false,
                        bytes_completed: total_bytes,
                        files_completed: item_count as u64,
                    }),
                    _ => {
                        // Sentinel meaning "fall back to the normal path".
                        Ok(PendingItemResult {
                            skipped: false,
                            bytes_completed: 0,
                            files_completed: 0, // 0 requests the fallback
                        })
                    }
                }
            },
            move |result| Message::RsyncBatchCompleted(job_id.clone(), item_count, result),
        );
    }

    let max_concurrent_transfers = max_concurrent_transfers_for_items(
        &pending_snapshot.items[pending_snapshot.current_index..],
    );
    let batch_size = remaining.min(max_concurrent_transfers.max(1));

    let mut tasks = Vec::with_capacity(batch_size);
    for offset in 0..batch_size {
        let idx = pending_snapshot.current_index + offset;
        let item = pending_snapshot.items[idx].clone();
        tasks.push(spawn_apply_one_item(
            &pending_snapshot.job_id,
            idx,
            &item,
            pending_snapshot.is_move,
            resolution.clone(),
            base_bytes,
        ));
    }

    if let Some(pending) = &mut state.pending_operation {
        pending.current_index += batch_size;
        pending.in_flight = batch_size;
    }

    Task::batch(tasks)
}

pub fn apply_conflict_resolution(
    state: &mut AppState,
    resolution: ConflictResolution,
) -> Task<Message> {
    let pending_snapshot = match &state.pending_operation {
        Some(p) => p.clone(),
        None => return Task::none(),
    };

    if pending_snapshot.current_index >= pending_snapshot.items.len() {
        return Task::none();
    }
    let item = pending_snapshot.items[pending_snapshot.current_index].clone();
    let task = spawn_apply_one_item(
        &pending_snapshot.job_id,
        pending_snapshot.current_index,
        &item,
        pending_snapshot.is_move,
        resolution,
        state
            .jobs
            .iter()
            .find(|job| job.id == pending_snapshot.job_id)
            .map(|job| job.progress.bytes_completed)
            .unwrap_or(0),
    );
    if let Some(pending) = &mut state.pending_operation {
        pending.current_index += 1;
        pending.in_flight = 1;
    }
    task
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
                    skipped: outcome.skipped,
                    bytes_completed: outcome.bytes_completed,
                    files_completed: 1,
                })
        },
        move |result| Message::PendingItemCompleted(callback_job_id, result),
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
