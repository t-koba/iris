use super::*;
use crate::i18n::{tr, tr_with};
use crate::state::PendingUndoAction;
use crate::undo::UndoableOp;

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
    // Rename (rsync cannot rename). A batch reports only aggregate success, so an undoable job
    // with Skip must use the normal per-item path to learn which destinations were left alone.
    let remaining = pending.items.len() - pending.current_index;
    if remaining >= 2
        && pending.in_flight == 0
        && pending.retry.is_empty()
        && !matches!(resolution, ConflictResolution::Rename)
        && !(pending.undo_action.is_some() && matches!(resolution, ConflictResolution::Skip))
        && !pending.rsync_failed
    {
        let items = pending.items[pending.current_index..].to_vec();
        let job_id = pending.job_id.clone();
        let is_move = pending.is_move;
        let item_count = items.len();
        let dispatch_generation = app_runtime().cancel_generation(&job_id);
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
                        applied: true,
                        installed_path: None,
                        // The aggregate carries no path. On success the handler fills each
                        // batch item's installed path from the proven branch state, where
                        // neither Rename nor Skip can change the requested destination.
                        undoable: false,
                    }),
                    // Sentinel meaning "fall back to the normal path".
                    _ => Ok(PendingItemResult {
                        bytes_completed: 0,
                        files_completed: 0,
                        applied: true,
                        installed_path: None,
                        undoable: false,
                    }),
                }
            },
            move |result| {
                Message::RsyncBatchCompleted(
                    job_id.clone(),
                    item_count,
                    dispatch_generation,
                    result,
                )
            },
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

/// An undo or redo op is replayed as one pending item per op entry, so skipped indices
/// refer to the same entries even when some of them were left untouched. When a conflict
/// installed a numbered sibling, the opposite journal must record that path instead of the
/// requested one.
fn undo_op_after_replay(
    op: &UndoableOp,
    pending: &PendingFileOperation,
    rewrite_source: bool,
) -> UndoableOp {
    let items = op
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            if pending.skipped.contains(&index)
                || pending.items.get(index).is_some_and(|item| !item.undoable)
            {
                return None;
            }
            let mut item = item.clone();
            if let Some(installed) = pending
                .items
                .get(index)
                .and_then(|item| item.installed_path.as_deref())
            {
                if rewrite_source {
                    item.source_path = PathBuf::from(installed);
                } else {
                    item.destination_path = PathBuf::from(installed);
                }
            }
            Some(item)
        })
        .collect();
    UndoableOp {
        items,
        ..op.clone()
    }
}

/// The part of an undo/redo replay that left its destination untouched. It must remain in the
/// journal entry it came from, otherwise a skipped item cannot be retried later. An applied item
/// that is deliberately not undoable (for example a merged folder) belongs in neither stack.
fn unreplayed_op(op: &UndoableOp, pending: &PendingFileOperation) -> UndoableOp {
    let items = op
        .items
        .iter()
        .enumerate()
        .filter(|(index, _)| pending.skipped.contains(index))
        .map(|(_, item)| item.clone())
        .collect();
    UndoableOp {
        items,
        ..op.clone()
    }
}

fn finish_pending_operation(state: &mut AppState) -> Task<Message> {
    let Some(pending) = state.pending_operation.take() else {
        return Task::none();
    };
    let mut completion_tasks = vec![Task::done(Message::Refresh)];
    match &pending.undo_action {
        Some(PendingUndoAction::RecordTransfer) => {
            let undo_kind = if pending.is_move {
                UndoOpKind::Move
            } else {
                UndoOpKind::Copy
            };
            let undo_items = pending
                .applied_items()
                .filter(|item| item.undoable)
                .filter_map(|item| {
                    // An installed path is the proof that the target belongs to this
                    // operation; the requested destination alone is not ownership.
                    let installed = item.installed_path.as_deref()?;
                    Some(UndoItem {
                        source_path: PathBuf::from(&item.source.display_path),
                        destination_path: PathBuf::from(installed),
                        size: None,
                        mtime: None,
                        trashed_path: None,
                        trash_id: None,
                        normalized_path: None,
                    })
                })
                .collect();
            completion_tasks.push(record_undo_op_task(undo_kind, undo_items));
        }
        Some(PendingUndoAction::RecordOp(op)) => {
            // Redo installed the file at its (possibly numbered) destination.
            let replayed = undo_op_after_replay(op, &pending, false);
            if !replayed.items.is_empty() {
                state.undo_journal.push_undo_from_redo(replayed);
            }
            // A skipped replay item was never redone, so it stays available in redo.
            let remaining = unreplayed_op(op, &pending);
            if !remaining.items.is_empty() {
                state.undo_journal.push_redo(remaining);
            }
        }
        Some(PendingUndoAction::PushRedo(op)) => {
            // Undo moved the file to its (possibly numbered) destination.
            let replayed = undo_op_after_replay(op, &pending, true);
            if !replayed.items.is_empty() {
                state.undo_journal.push_redo(replayed);
            }
            // A skipped replay item was never undone, so it stays available in undo.
            let remaining = unreplayed_op(op, &pending);
            if !remaining.items.is_empty() {
                state.undo_journal.push_undo_from_redo(remaining);
            }
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
            pending.mark_skipped(index);
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

/// Replay entries are popped from the journal before their transfer starts. An abort with no
/// confirmed installation has not changed any destination, so the entry must remain retryable.
/// A confirmed installation means the replay was partly applied; its journal outcome cannot be
/// reconstructed from the abort alone, so the stacks are left untouched.
pub(crate) enum ReplayRestore {
    Undo(UndoableOp),
    Redo(UndoableOp),
}

pub(crate) fn replay_entry_to_restore(pending: &PendingFileOperation) -> Option<ReplayRestore> {
    if pending
        .items
        .iter()
        .any(|item| item.installed_path.is_some())
    {
        return None;
    }
    match &pending.undo_action {
        // Redo replay popped the entry from the redo stack.
        Some(PendingUndoAction::RecordOp(op)) => Some(ReplayRestore::Redo(op.clone())),
        // Undo replay popped the entry from the undo stack.
        Some(PendingUndoAction::PushRedo(op)) => Some(ReplayRestore::Undo(op.clone())),
        _ => None,
    }
}

pub(crate) fn restore_replay_entry(state: &mut AppState, replay: ReplayRestore) {
    match replay {
        ReplayRestore::Undo(op) => state.undo_journal.push_undo_from_redo(op),
        ReplayRestore::Redo(op) => state.undo_journal.push_redo(op),
    }
}

/// Cancels a job in the runtime and marks it canceled in the UI state.
pub fn cancel_job_state(state: &mut AppState, job_id: &JobId) {
    dismiss_job_decisions(state, job_id);
    app_runtime().cancel_job(job_id);
    if let Some(job) = state.jobs.iter_mut().find(|job| job.id == *job_id) {
        job.state = JobState::Canceled;
    }
    let replay = state
        .pending_operation
        .as_ref()
        .filter(|pending| pending.job_id == *job_id)
        .and_then(replay_entry_to_restore);
    if let Some(action) = replay {
        restore_replay_entry(state, action);
    }
    if state
        .pending_operation
        .as_ref()
        .is_some_and(|pending| pending.job_id == *job_id)
    {
        state.pending_operation = None;
        state.active_dialog = None;
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
    let callback_job_id = job_id.clone();
    // Capture the lifecycle before handing the future to the task scheduler.
    let runtime = app_runtime();
    let dispatch_generation = runtime.cancel_generation(job_id);
    let transfer = Arc::clone(runtime).apply_pending_item(
        job_id.clone(),
        task_index,
        item.clone(),
        is_move,
        resolution,
        base_bytes_completed,
    );

    Task::perform(
        async move {
            transfer.await.map(|outcome| PendingItemResult {
                bytes_completed: outcome.bytes_completed,
                files_completed: 1,
                applied: outcome.applied,
                installed_path: outcome.installed_path,
                undoable: outcome.undoable,
            })
        },
        move |result| {
            Message::PendingItemCompleted(callback_job_id, task_index, dispatch_generation, result)
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PendingUndoAction;
    use crate::test_support::local_entry;
    use iris_core_domain::types::transfer::JobId;

    #[test]
    fn applied_item_without_installed_path_is_not_journaled() {
        let mut state = AppState::default();
        let job_id = JobId(9_700_000);
        let mut pending = PendingFileOperation::new(
            job_id.clone(),
            vec![PendingFileItem {
                source: local_entry("/src/file.txt", false).object_ref,
                destination_path: "/dst/file.txt".to_owned(),
                installed_path: None,
                undoable: true,
            }],
            false,
            Some(PendingUndoAction::RecordTransfer),
            1,
        );
        pending.current_index = 1;
        state.pending_operation = Some(pending);

        let _ = finish_pending_operation(&mut state);

        assert!(
            !state.undo_journal.can_undo(),
            "the requested destination is not proof of an installed path"
        );
    }
}
