use super::file_ops;
use super::file_ops::*;
use super::undo_ops::*;
use super::update;
use crate::i18n::{tr, tr_args};
use crate::messages::{Message, PaneMessage, SidebarMessage};
use crate::runtime::app_runtime;
use crate::state::{
    AppState, BatchRenameDialogState, BatchRenameEntry, BatchRenameMode, BatchRenamePreviewItem,
    ClipboardContent, ContextMenuKind, ContextMenuState, DialogState, NotificationAction,
    NotificationSeverity, PendingFileItem,
};
use crate::undo::{UndoItem, UndoOpKind};
use chrono::{DateTime, Local};
use fluent_bundle::{FluentArgs, FluentValue};
use iced::Task;
use iris_core_domain::types::object::ObjectRef;
use iris_core_domain::types::options::ConflictResolution;
use iris_core_domain::types::transfer::JobState;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug)]
struct BatchRenameOperation {
    target: ObjectRef,
    new_name: String,
    original_path: PathBuf,
    renamed_path: PathBuf,
}

#[derive(Clone, Debug)]
struct BatchRenameRunResult {
    items: Vec<UndoItem>,
    failures: Vec<String>,
}

fn begin_batch_rename(state: &mut AppState) {
    let pane_idx = state.active_pane;
    let pane = state.pane(pane_idx);
    let mut indices = pane.selected_indices.iter().copied().collect::<Vec<_>>();
    indices.sort_by_key(|index| pane.visible_position(*index).unwrap_or(*index as usize));

    let items = indices
        .into_iter()
        .filter_map(|index| pane.entry(index))
        .map(|entry| BatchRenameEntry {
            target: entry.object_ref.clone(),
            original_name: entry.name.clone(),
            original_path: PathBuf::from(&entry.object_ref.display_path),
            mtime: entry.mtime,
        })
        .collect::<Vec<_>>();
    if items.len() < 2 {
        return;
    }

    let existing_paths = pane
        .entries
        .iter()
        .map(|entry| PathBuf::from(&entry.object_ref.display_path))
        .collect::<Vec<_>>();
    let mut dialog = BatchRenameDialogState {
        pane: pane_idx,
        items,
        existing_paths,
        mode: BatchRenameMode::NumberSequence,
        pattern_input: String::new(),
        replacement_input: String::new(),
        start_input: "1".to_string(),
        padding_input: "2".to_string(),
        date_format_input: "%Y%m%d".to_string(),
        preview: Vec::new(),
        error_message: None,
    };
    refresh_batch_rename_preview(&mut dialog);
    state.active_dialog = Some(DialogState::BatchRename { state: dialog });
}

fn active_batch_rename_dialog(state: &AppState) -> Option<&BatchRenameDialogState> {
    match &state.active_dialog {
        Some(DialogState::BatchRename { state }) => Some(state),
        _ => None,
    }
}

fn active_batch_rename_dialog_mut(state: &mut AppState) -> Option<&mut BatchRenameDialogState> {
    match &mut state.active_dialog {
        Some(DialogState::BatchRename { state }) => Some(state),
        _ => None,
    }
}

fn update_batch_rename_dialog(
    state: &mut AppState,
    update: impl FnOnce(&mut BatchRenameDialogState),
) {
    let Some(dialog) = active_batch_rename_dialog_mut(state) else {
        return;
    };
    update(dialog);
    refresh_batch_rename_preview(dialog);
}

fn refresh_batch_rename_preview(dialog: &mut BatchRenameDialogState) {
    let (preview, error_message) = build_batch_rename_preview(dialog);
    dialog.preview = preview;
    dialog.error_message = error_message;
}

fn build_batch_rename_preview(
    dialog: &BatchRenameDialogState,
) -> (Vec<BatchRenamePreviewItem>, Option<String>) {
    let generator = match BatchRenameGenerator::new(dialog) {
        Ok(generator) => generator,
        Err(error) => {
            return (
                dialog
                    .items
                    .iter()
                    .map(|item| BatchRenamePreviewItem {
                        original_name: item.original_name.clone(),
                        new_name: item.original_name.clone(),
                        status: Some(error.clone()),
                        blocking: true,
                    })
                    .collect(),
                Some(error),
            );
        }
    };

    let existing_paths = dialog
        .existing_paths
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let mut preview = dialog
        .items
        .iter()
        .enumerate()
        .map(|(position, item)| {
            let new_name = generator.new_name(item, position);
            let mut status = None;
            let mut blocking = false;
            if let Some(error) = crate::format::file_name_error(&new_name) {
                status = Some(error);
                blocking = true;
            } else if new_name == item.original_name {
                status = Some(tr("batch-rename-status-unchanged"));
            }
            BatchRenamePreviewItem {
                original_name: item.original_name.clone(),
                new_name,
                status,
                blocking,
            }
        })
        .collect::<Vec<_>>();

    let mut destinations: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (index, (item, preview_item)) in dialog.items.iter().zip(preview.iter()).enumerate() {
        if preview_item.blocking {
            continue;
        }
        destinations
            .entry(item.original_path.with_file_name(&preview_item.new_name))
            .or_default()
            .push(index);
    }

    for (path, indices) in &destinations {
        if indices.len() > 1 {
            for index in indices {
                preview[*index].status = Some(tr("batch-rename-status-duplicate"));
                preview[*index].blocking = true;
            }
            continue;
        }

        let index = indices[0];
        if existing_paths.contains(path) && path != &dialog.items[index].original_path {
            preview[index].status = Some(tr("batch-rename-status-conflict"));
            preview[index].blocking = true;
        }
    }

    (preview, None)
}

enum BatchRenameGenerator {
    NumberSequence {
        base: String,
        start: i64,
        padding: usize,
    },
    Replace {
        find: String,
        replacement: String,
    },
    Regex {
        regex: Regex,
        replacement: String,
    },
    Date {
        base: String,
        format: String,
    },
}

impl BatchRenameGenerator {
    fn new(dialog: &BatchRenameDialogState) -> Result<Self, String> {
        match dialog.mode {
            BatchRenameMode::NumberSequence => Ok(Self::NumberSequence {
                base: dialog.replacement_input.clone(),
                start: parse_batch_rename_i64(
                    &dialog.start_input,
                    "batch-rename-error-invalid-start",
                )?,
                padding: parse_batch_rename_usize(
                    &dialog.padding_input,
                    "batch-rename-error-invalid-padding",
                )?,
            }),
            BatchRenameMode::Replace => {
                if dialog.pattern_input.is_empty() {
                    return Err(tr("batch-rename-error-empty-find"));
                }
                Ok(Self::Replace {
                    find: dialog.pattern_input.clone(),
                    replacement: dialog.replacement_input.clone(),
                })
            }
            BatchRenameMode::Regex => {
                if dialog.pattern_input.is_empty() {
                    return Err(tr("batch-rename-error-empty-regex"));
                }
                let regex = Regex::new(&dialog.pattern_input)
                    .map_err(|error| format!("{}: {error}", tr("batch-rename-error-regex")))?;
                Ok(Self::Regex {
                    regex,
                    replacement: dialog.replacement_input.clone(),
                })
            }
            BatchRenameMode::Date => {
                if dialog.date_format_input.trim().is_empty() {
                    return Err(tr("batch-rename-error-empty-date-format"));
                }
                Ok(Self::Date {
                    base: dialog.replacement_input.clone(),
                    format: dialog.date_format_input.clone(),
                })
            }
        }
    }

    fn new_name(&self, item: &BatchRenameEntry, position: usize) -> String {
        match self {
            Self::NumberSequence {
                base,
                start,
                padding,
            } => {
                let (stem, ext) = split_file_name(&item.original_name);
                let prefix = if base.is_empty() { stem } else { base.clone() };
                let number = start + position as i64;
                format!("{prefix}{}{ext}", format_padded_number(number, *padding))
            }
            Self::Replace { find, replacement } => item.original_name.replace(find, replacement),
            Self::Regex { regex, replacement } => regex
                .replace_all(&item.original_name, replacement.as_str())
                .to_string(),
            Self::Date { base, format } => {
                let (stem, ext) = split_file_name(&item.original_name);
                let prefix = if base.is_empty() { stem } else { base.clone() };
                let timestamp = item.mtime.unwrap_or_else(SystemTime::now);
                let date: DateTime<Local> = DateTime::from(timestamp);
                format!("{prefix}{}{ext}", date.format(format))
            }
        }
    }
}

fn parse_batch_rename_i64(value: &str, error_key: &str) -> Result<i64, String> {
    value.trim().parse::<i64>().map_err(|_| tr(error_key))
}

fn parse_batch_rename_usize(value: &str, error_key: &str) -> Result<usize, String> {
    value
        .trim()
        .parse::<usize>()
        .map(|value| value.min(12))
        .map_err(|_| tr(error_key))
}

fn split_file_name(name: &str) -> (String, String) {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string());
    let ext = path
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy()))
        .unwrap_or_default();
    (stem, ext)
}

fn format_padded_number(number: i64, padding: usize) -> String {
    if padding == 0 {
        number.to_string()
    } else if number < 0 {
        format!("-{:0width$}", number.abs(), width = padding)
    } else {
        format!("{number:0width$}", width = padding)
    }
}

fn batch_rename_operations(dialog: &BatchRenameDialogState) -> Vec<BatchRenameOperation> {
    dialog
        .items
        .iter()
        .zip(&dialog.preview)
        .filter(|(item, preview)| !preview.blocking && preview.new_name != item.original_name)
        .map(|(item, preview)| BatchRenameOperation {
            target: item.target.clone(),
            new_name: preview.new_name.clone(),
            original_path: item.original_path.clone(),
            renamed_path: item.original_path.with_file_name(&preview.new_name),
        })
        .collect()
}

async fn run_batch_rename(operations: Vec<BatchRenameOperation>) -> BatchRenameRunResult {
    let mut items = Vec::new();
    let mut failures = Vec::new();
    for operation in operations {
        match app_runtime()
            .rename_object(&operation.target, &operation.new_name)
            .await
        {
            Ok(()) => items.push(UndoItem {
                source_path: operation.original_path,
                destination_path: operation.renamed_path,
                size: None,
                mtime: None,
                trashed_path: None,
                trash_id: None,
                normalized_path: None,
            }),
            Err(error) => failures.push(format!(
                "{}: {error}",
                operation
                    .original_path
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
                    .unwrap_or_else(|| operation.original_path.display().to_string())
            )),
        }
    }
    BatchRenameRunResult { items, failures }
}

fn batch_rename_failure_message(count: usize, first_failure: Option<&String>) -> String {
    if count == 1 {
        first_failure
            .cloned()
            .unwrap_or_else(|| tr("notification-batch-rename-failed-body"))
    } else {
        let mut args = FluentArgs::new();
        args.set("count", FluentValue::from(count as i64));
        tr_args("notification-batch-rename-failed-many", Some(&args))
    }
}

pub(super) fn handle(state: &mut AppState, message: Message) -> Task<Message> {
    match message {
        Message::CopySelected => {
            let pi = state.active_pane;
            let refs = state.pane(pi).selected_object_refs();
            if !refs.is_empty() {
                state.clipboard = Some(ClipboardContent::Copy(refs));
            }
        }
        Message::CutSelected => {
            let pi = state.active_pane;
            let refs = state.pane(pi).selected_object_refs();
            if !refs.is_empty() {
                state.clipboard = Some(ClipboardContent::Cut(refs));
            }
        }
        Message::Paste => {
            if let Some(ref clipboard) = state.clipboard {
                let (refs, is_move) = match clipboard {
                    ClipboardContent::Copy(refs) => (refs.clone(), false),
                    ClipboardContent::Cut(refs) => (refs.clone(), true),
                };
                if !refs.is_empty() {
                    let destination_dir = PathBuf::from(&state.active_pane().current_path);
                    let items: Vec<PendingFileItem> = refs
                        .iter()
                        .map(|source| PendingFileItem {
                            source: source.clone(),
                            destination_path: destination_dir
                                .join(file_name_from_path(&source.display_path))
                                .to_string_lossy()
                                .to_string(),
                        })
                        .collect();
                    let task = start_pending_file_operation(state, items, is_move);

                    if is_move {
                        state.clipboard = None;
                    }
                    return task;
                }
            }
        }
        Message::DeleteSelected => {
            let pi = state.active_pane;
            let refs = state.pane(pi).selected_object_refs();
            let names = state.pane(pi).selected_names();
            if !refs.is_empty() {
                let can_use_trash = refs.iter().all(|r| r.key.backend_id.0.starts_with("local"));
                state.active_dialog = Some(DialogState::DeleteConfirmation {
                    targets: refs,
                    target_names: names,
                    use_trash: can_use_trash,
                    can_use_trash,
                });
            }
        }
        Message::Undo => {
            let Some(op) = state.undo_journal.peek_undo().cloned() else {
                return Task::none();
            };
            if undo_requires_remote_confirmation(&op) {
                if !state.remote_copy_undo_enabled {
                    state.add_notification(
                        NotificationSeverity::Warning,
                        tr("notification-remote-undo-disabled-title"),
                        tr("notification-remote-undo-disabled-body"),
                        vec![NotificationAction::Dismiss],
                    );
                    return Task::none();
                }
                state.active_dialog = Some(DialogState::RemoteUndoConfirmation {
                    item_count: op.items.len(),
                    op,
                });
                return Task::none();
            }
            let Some(op) = state.undo_journal.pop_undo() else {
                return Task::none();
            };
            return perform_undo(state, op);
        }
        Message::ConfirmRemoteUndo => {
            let expected_op = match &state.active_dialog {
                Some(DialogState::RemoteUndoConfirmation { op, .. }) => Some(op.clone()),
                _ => None,
            };
            state.active_dialog = None;
            let Some(expected_op) = expected_op else {
                return Task::none();
            };
            let Some(op) = state.undo_journal.pop_undo() else {
                return Task::none();
            };
            if op != expected_op {
                state.undo_journal.push_undo_from_redo(op);
                return Task::none();
            }
            return perform_undo(state, expected_op);
        }
        Message::Redo => {
            let Some(op) = state.undo_journal.pop_redo() else {
                return Task::none();
            };
            return perform_redo(state, op);
        }
        Message::OpenSelected => {
            let pi = state.active_pane;
            let refs = state.pane(pi).selected_object_refs();
            if !refs.is_empty() {
                return Task::batch(refs.into_iter().map(open_object_task));
            }
        }
        Message::CreateFolder => {
            state.active_dialog = Some(DialogState::CreateFolder {
                input_value: String::new(),
            });
        }
        Message::Rename(index) => {
            let pi = state.active_pane;
            state.active_dialog = None;
            if let Some(target) = state.pane_mut(pi).begin_inline_rename(index) {
                let input_id = crate::app::ids::inline_rename_input_id(pi, &target.display_path);
                return Task::batch([
                    iced::widget::operation::focus(input_id.clone()),
                    iced::widget::operation::select_all(input_id),
                ]);
            }
        }
        Message::BatchRenameSelected => {
            begin_batch_rename(state);
        }
        Message::BatchRenameModeChanged(mode) => {
            update_batch_rename_dialog(state, |dialog| {
                dialog.mode = mode;
            });
        }
        Message::BatchRenamePatternChanged(value) => {
            update_batch_rename_dialog(state, |dialog| {
                dialog.pattern_input = value;
            });
        }
        Message::BatchRenameReplacementChanged(value) => {
            update_batch_rename_dialog(state, |dialog| {
                dialog.replacement_input = value;
            });
        }
        Message::BatchRenameStartChanged(value) => {
            update_batch_rename_dialog(state, |dialog| {
                dialog.start_input = value;
            });
        }
        Message::BatchRenamePaddingChanged(value) => {
            update_batch_rename_dialog(state, |dialog| {
                dialog.padding_input = value;
            });
        }
        Message::BatchRenameDateFormatChanged(value) => {
            update_batch_rename_dialog(state, |dialog| {
                dialog.date_format_input = value;
            });
        }
        Message::PerformBatchRename => {
            let Some(dialog) = active_batch_rename_dialog(state).cloned() else {
                return Task::none();
            };
            if !dialog.can_apply() {
                return Task::none();
            }
            let operations = batch_rename_operations(&dialog);
            if operations.is_empty() {
                return Task::none();
            }
            state.active_dialog = None;
            return Task::perform(run_batch_rename(operations), |result| {
                Message::BatchRenameCompleted {
                    items: result.items,
                    failures: result.failures,
                }
            });
        }
        Message::RenameCompleted {
            original_path,
            renamed_path,
            result,
        } => {
            if let Err(e) = result {
                state.add_notification(
                    NotificationSeverity::Error,
                    tr("notification-rename-failed-title"),
                    e,
                    vec![NotificationAction::Dismiss],
                );
            } else {
                return Task::batch([
                    record_undo_op_task(
                        UndoOpKind::Rename,
                        vec![UndoItem {
                            source_path: original_path,
                            destination_path: renamed_path,
                            size: None,
                            mtime: None,
                            trashed_path: None,
                            trash_id: None,
                            normalized_path: None,
                        }],
                    ),
                    update(state, Message::Refresh),
                ]);
            }
            return update(state, Message::Refresh);
        }
        Message::BatchRenameCompleted { items, failures } => {
            if !failures.is_empty() {
                state.add_notification(
                    if items.is_empty() {
                        NotificationSeverity::Error
                    } else {
                        NotificationSeverity::Warning
                    },
                    tr("notification-batch-rename-failed-title"),
                    batch_rename_failure_message(failures.len(), failures.first()),
                    vec![NotificationAction::Dismiss],
                );
            }
            let mut tasks = vec![update(state, Message::Refresh)];
            if !items.is_empty() {
                tasks.push(record_undo_op_task(UndoOpKind::Rename, items));
            }
            return Task::batch(tasks);
        }
        Message::PerformCreateFolder(name) => {
            if crate::format::file_name_error(&name).is_some() {
                // The dialog shows the problem and keeps Create disabled.
                return Task::none();
            }
            let current_path = state.active_pane().current_path.clone();
            let created_path = PathBuf::from(&current_path).join(&name);
            state.active_dialog = None;
            return Task::perform(
                async move { app_runtime().create_folder(&current_path, &name).await },
                move |result| Message::CreateFolderCompleted {
                    path: created_path,
                    result,
                },
            );
        }
        Message::CreateFolderCompleted { path, result } => {
            if let Err(e) = result {
                state.add_notification(
                    NotificationSeverity::Error,
                    tr("notification-create-folder-failed-title"),
                    e,
                    vec![NotificationAction::Dismiss],
                );
            } else {
                return Task::batch([
                    record_undo_op_task(
                        UndoOpKind::CreateFolder,
                        vec![UndoItem {
                            source_path: path.clone(),
                            destination_path: path,
                            size: None,
                            mtime: None,
                            trashed_path: None,
                            trash_id: None,
                            normalized_path: None,
                        }],
                    ),
                    update(state, Message::Refresh),
                ]);
            }
            return update(state, Message::Refresh);
        }
        Message::PerformDelete => {
            let delete_info =
                if let Some(DialogState::DeleteConfirmation {
                    targets, use_trash, ..
                }) = &state.active_dialog
                {
                    Some((targets.clone(), *use_trash))
                } else {
                    None
                };

            state.active_dialog = None;
            if let Some((targets, use_trash)) = delete_info {
                state.active_pane_mut().clear_selection();
                if use_trash {
                    let paths = targets
                        .iter()
                        .map(|target| PathBuf::from(&target.display_path))
                        .collect();
                    return Task::perform(trash_paths_for_undo(paths), move |result| {
                        let trashed_items = result.clone().unwrap_or_default();
                        Message::DeleteCompleted {
                            trashed_items,
                            result: result.map(|_| ()),
                        }
                    });
                }
                return Task::perform(
                    async move { app_runtime().delete_objects(&targets, use_trash).await },
                    move |result| Message::DeleteCompleted {
                        trashed_items: Vec::new(),
                        result,
                    },
                );
            }
            state.active_pane_mut().clear_selection();
            return update(state, Message::Refresh);
        }
        Message::OpenObjectCompleted(Err(error)) => {
            state.add_notification(
                NotificationSeverity::Error,
                tr("notification-open-default-app-failed-title"),
                error,
                vec![NotificationAction::Dismiss],
            );
        }
        Message::DeleteCompleted {
            trashed_items,
            result,
        } => {
            if let Err(e) = result {
                state.add_notification(
                    NotificationSeverity::Error,
                    tr("notification-delete-failed-title"),
                    e,
                    vec![NotificationAction::Dismiss],
                );
            } else if !trashed_items.is_empty() {
                return Task::batch([
                    record_undo_op_task(UndoOpKind::TrashDelete, trashed_items),
                    update(state, Message::Refresh),
                ]);
            }
            return update(state, Message::Refresh);
        }
        Message::RecordUndoOp { kind, items } => {
            record_undo_op(state, kind, items);
        }
        Message::UndoCompleted {
            op,
            redo_op,
            result,
        } => {
            match result {
                Ok(()) => {
                    if let Some(redo_op) = redo_op {
                        state.undo_journal.push_redo(redo_op);
                    }
                    state.add_notification(
                        NotificationSeverity::Info,
                        tr("notification-undo-completed-title"),
                        undo_success_message(&op),
                        vec![NotificationAction::Dismiss],
                    );
                }
                Err(error) => {
                    state.add_notification(
                        NotificationSeverity::Warning,
                        tr("notification-undo-failed-title"),
                        error,
                        vec![NotificationAction::Dismiss],
                    );
                    drop(op);
                }
            }
            return update(state, Message::Refresh);
        }
        Message::RedoCompleted {
            op,
            undo_op,
            result,
        } => {
            match result {
                Ok(()) => {
                    if let Some(undo_op) = undo_op {
                        state.undo_journal.push_undo_from_redo(undo_op);
                    }
                    state.add_notification(
                        NotificationSeverity::Info,
                        tr("notification-redo-completed-title"),
                        redo_success_message(&op),
                        vec![NotificationAction::Dismiss],
                    );
                }
                Err(error) => {
                    state.add_notification(
                        NotificationSeverity::Warning,
                        tr("notification-redo-failed-title"),
                        error,
                        vec![NotificationAction::Dismiss],
                    );
                    drop(op);
                }
            }
            return update(state, Message::Refresh);
        }
        Message::RsyncBatchCompleted(job_id, item_count, result) => {
            match result {
                Ok(item_result) if item_result.files_completed > 0 => {
                    // rsync transferred everything.
                    if let Some(job) = state.jobs.iter_mut().find(|j| j.id == job_id) {
                        job.progress.files_completed += item_result.files_completed;
                        job.progress.bytes_completed += item_result.bytes_completed;
                        update_job_speed(job);
                    }
                    if let Some(pending) = &mut state.pending_operation {
                        if pending.job_id == job_id {
                            pending.in_flight = 0;
                        }
                    }
                    return process_pending_operation(state);
                }
                _ => {
                    // rsync unavailable or failed: rewind current_index and fall back to the normal path.
                    if let Some(pending) = &mut state.pending_operation {
                        if pending.job_id == job_id {
                            pending.current_index -= item_count;
                            pending.in_flight = 0;
                            pending.rsync_failed = true; // do not retry rsync
                        }
                    }
                    return process_pending_operation(state);
                }
            }
        }
        Message::PendingItemCompleted(job_id, index, result) => {
            let is_current_job = state
                .pending_operation
                .as_ref()
                .is_some_and(|pending| pending.job_id == job_id);
            match result {
                Ok(item_result) => {
                    if let Some(job) = state.jobs.iter_mut().find(|j| j.id == job_id) {
                        // Skipped files are done too, so progress still reaches the total.
                        job.progress.files_completed += 1;
                        job.progress.bytes_completed += item_result.bytes_completed;
                        // Estimate bytes_total from the average of the completed files.
                        if job.progress.bytes_total.is_none() && job.progress.files_completed > 0 {
                            if let Some(files_total) = job.progress.files_total {
                                let avg_per_file =
                                    job.progress.bytes_completed / job.progress.files_completed;
                                job.progress.bytes_total = Some(avg_per_file * files_total);
                            }
                        }
                        update_job_speed(job);
                    }
                    if !is_current_job {
                        return Task::none();
                    }
                    if let Some(pending) = &mut state.pending_operation {
                        pending.in_flight = pending.in_flight.saturating_sub(1);
                    }
                    return process_pending_operation(state);
                }
                Err(error) if error == "transfer canceled" => {
                    if let Some(job) = state.jobs.iter_mut().find(|j| j.id == job_id) {
                        job.state = JobState::Canceled;
                        reset_job_speed(job);
                    }
                    if is_current_job {
                        state.pending_operation = None;
                    }
                    return Task::none();
                }
                Err(error) => {
                    if !is_current_job {
                        return Task::none();
                    }
                    let first_failure = state.pending_operation.as_mut().is_some_and(|pending| {
                        pending.in_flight = pending.in_flight.saturating_sub(1);
                        pending.failed.push(index);
                        pending.failed.len() == 1
                    });
                    // Transfers already running finish; nothing new starts until the user
                    // decides. One notification covers every failure of the job.
                    if first_failure {
                        fail_pending_job(state, &job_id, &error);
                    }
                }
            }
        }
        Message::ConflictCheckCompleted(job_id, task_index, result) => {
            let Some(pending) = &mut state.pending_operation else {
                return Task::none();
            };
            if pending.job_id != job_id || pending.next_index() != Some(task_index) {
                return Task::none();
            }
            if result.is_err() {
                // The file cannot even be checked: it fails like a transfer, so Retry checks it
                // again and Skip moves past it.
                let _ = pending.take_next_index();
                pending.failed.push(task_index);
                pending.in_flight = 0;
            }
            match result {
                Ok(Some(conflict)) => {
                    state.active_dialog = Some(DialogState::Conflict {
                        source: conflict.source,
                        destination_path: conflict.destination_path,
                        source_size: conflict.source_size,
                        destination_size: conflict.destination_size,
                        source_mtime: conflict.source_mtime,
                        destination_mtime: conflict.destination_mtime,
                        remember_choice: false,
                    });
                }
                Ok(None) => {
                    return file_ops::apply_conflict_resolution(
                        state,
                        ConflictResolution::Overwrite,
                    );
                }
                Err(error) => {
                    fail_pending_job(state, &job_id, &error);
                }
            }
        }
        Message::SetDeleteUseTrash(use_trash) => {
            if let Some(DialogState::DeleteConfirmation {
                use_trash: current,
                can_use_trash,
                ..
            }) = &mut state.active_dialog
            {
                *current = *can_use_trash && use_trash;
            }
        }
        Message::OpenItemContextMenu { pane: pi, index } => {
            if let Some(entry) = state.pane(pi).entry(index) {
                let target_name = entry.name.clone();
                let position = state.mouse_position.unwrap_or(iced::Point::ORIGIN);
                state.active_pane = pi;
                state.pane_mut(pi).select_single(index);
                state.active_dialog = None;
                state.context_menu = Some(ContextMenuState {
                    kind: ContextMenuKind::Item { pane: pi, index },
                    target_name,
                    position,
                });
            }
        }
        Message::OpenSidebarContextMenu { group, item } => {
            if let Some(sidebar_group) = state.sidebar_groups.get(group) {
                if let Some(sidebar_item) = sidebar_group.items.get(item) {
                    let position = state.mouse_position.unwrap_or(iced::Point::ORIGIN);
                    state.active_dialog = None;
                    state.context_menu = Some(ContextMenuState {
                        kind: ContextMenuKind::SidebarItem {
                            group,
                            item,
                            removable: !sidebar_group.is_system(),
                        },
                        target_name: sidebar_item.name.clone(),
                        position,
                    });
                }
            }
        }
        Message::OpenTabContextMenu { pane, tab } => {
            if state.pane_checked(pane).is_some() && tab < state.pane_tabs(pane).tabs().len() {
                let tab_count = state.pane_tabs(pane).tabs().len();
                let target_name = tab_context_target_name(state, pane, tab);
                let position = state.mouse_position.unwrap_or(iced::Point::ORIGIN);
                state.active_dialog = None;
                state.context_menu = Some(ContextMenuState {
                    kind: ContextMenuKind::Tab {
                        pane,
                        tab,
                        can_close: tab_count > 1,
                        can_move_left: tab > 0,
                        can_move_right: tab + 1 < tab_count,
                    },
                    target_name,
                    position,
                });
            }
        }
        Message::CloseContextMenu => {
            state.context_menu = None;
        }
        Message::ItemActionOpen => {
            state.context_menu = None;
            return update(state, Message::OpenSelected);
        }
        Message::ItemActionCopy => {
            state.context_menu = None;
            return update(state, Message::CopySelected);
        }
        Message::ItemActionCut => {
            state.context_menu = None;
            return update(state, Message::CutSelected);
        }
        Message::ItemActionRename => {
            let target_index = match &state.context_menu {
                Some(ContextMenuState {
                    kind: ContextMenuKind::Item { index, .. },
                    ..
                }) => Some(*index),
                _ => state.active_pane().selected_indices.iter().copied().next(),
            };
            state.context_menu = None;
            if let Some(index) = target_index {
                return update(state, Message::Rename(index));
            }
        }
        Message::ItemActionDelete => {
            state.context_menu = None;
            return update(state, Message::DeleteSelected);
        }
        Message::SidebarContextOpen => {
            let target = sidebar_context_target_path(state);
            state.context_menu = None;
            if let Some(path) = target {
                return update(
                    state,
                    Message::Pane(state.active_pane, PaneMessage::NavigateToPath(path)),
                );
            }
        }
        Message::SidebarContextOpenInNewWindow => {
            let target = sidebar_context_target_path(state);
            state.context_menu = None;
            if let Some(path) = target {
                return update(state, SidebarMessage::OpenInNewWindow(path).into());
            }
        }
        _ => {}
    };
    Task::none()
}

fn sidebar_context_target_path(state: &AppState) -> Option<String> {
    match state.context_menu.as_ref() {
        Some(ContextMenuState {
            kind: ContextMenuKind::SidebarItem { group, item, .. },
            ..
        }) => state
            .sidebar_groups
            .get(*group)
            .and_then(|sidebar_group| sidebar_group.items.get(*item))
            .map(|sidebar_item| sidebar_item.path.clone()),
        _ => None,
    }
}

fn tab_context_target_name(state: &AppState, pane: usize, tab: usize) -> String {
    let Some(path) = state
        .pane_tabs(pane)
        .tabs()
        .get(tab)
        .map(|tab| tab.current_path.as_str())
    else {
        return tr("tab-untitled");
    };
    if path.is_empty() {
        return tr("tab-untitled");
    }
    let display_path = app_runtime().format_display_path(path);
    Path::new(&display_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(display_path.as_str())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core_domain::types::object::{BackendId, ObjectKey, StableId};

    fn object_ref(path: &str) -> ObjectRef {
        ObjectRef {
            key: ObjectKey {
                backend_id: BackendId("local:test".to_string()),
                stable_id: StableId(path.to_string()),
            },
            parent_stable_id: None,
            display_path: path.to_string(),
        }
    }

    fn batch_item(path: &str, name: &str) -> BatchRenameEntry {
        BatchRenameEntry {
            target: object_ref(path),
            original_name: name.to_string(),
            original_path: PathBuf::from(path),
            mtime: None,
        }
    }

    fn dialog_with_items(items: Vec<BatchRenameEntry>) -> BatchRenameDialogState {
        let existing_paths = items
            .iter()
            .map(|item| item.original_path.clone())
            .collect::<Vec<_>>();
        BatchRenameDialogState {
            pane: 0,
            items,
            existing_paths,
            mode: BatchRenameMode::NumberSequence,
            pattern_input: String::new(),
            replacement_input: String::new(),
            start_input: "1".to_string(),
            padding_input: "2".to_string(),
            date_format_input: "%Y%m%d".to_string(),
            preview: Vec::new(),
            error_message: None,
        }
    }

    #[test]
    fn batch_rename_number_preview_preserves_extensions_and_builds_operations() {
        let mut dialog = dialog_with_items(vec![
            batch_item("/tmp/a.txt", "a.txt"),
            batch_item("/tmp/b.txt", "b.txt"),
        ]);
        dialog.replacement_input = "Photo ".to_string();
        dialog.start_input = "7".to_string();
        dialog.padding_input = "3".to_string();
        refresh_batch_rename_preview(&mut dialog);

        assert!(dialog.can_apply());
        assert_eq!(dialog.preview[0].new_name, "Photo 007.txt");
        assert_eq!(dialog.preview[1].new_name, "Photo 008.txt");
        let operations = batch_rename_operations(&dialog);
        assert_eq!(operations.len(), 2);
        assert_eq!(
            operations[0].renamed_path,
            PathBuf::from("/tmp/Photo 007.txt")
        );
    }

    #[test]
    fn batch_rename_regex_preview_uses_capture_replacements() {
        let mut dialog = dialog_with_items(vec![
            batch_item("/tmp/IMG_0001.jpg", "IMG_0001.jpg"),
            batch_item("/tmp/IMG_0002.jpg", "IMG_0002.jpg"),
        ]);
        dialog.mode = BatchRenameMode::Regex;
        dialog.pattern_input = r"IMG_(\d+)".to_string();
        dialog.replacement_input = "Photo_$1".to_string();
        refresh_batch_rename_preview(&mut dialog);

        assert!(dialog.can_apply());
        assert_eq!(dialog.preview[0].new_name, "Photo_0001.jpg");
        assert_eq!(dialog.preview[1].new_name, "Photo_0002.jpg");
    }

    #[test]
    fn batch_rename_preview_blocks_existing_destination() {
        let mut dialog = dialog_with_items(vec![
            batch_item("/tmp/a.txt", "a.txt"),
            batch_item("/tmp/c.txt", "c.txt"),
        ]);
        dialog.existing_paths.push(PathBuf::from("/tmp/b.txt"));
        dialog.mode = BatchRenameMode::Replace;
        dialog.pattern_input = "a".to_string();
        dialog.replacement_input = "b".to_string();
        refresh_batch_rename_preview(&mut dialog);

        assert!(!dialog.can_apply());
        assert!(dialog.preview[0].blocking);
        assert_eq!(
            dialog.preview[0].status,
            Some(tr("batch-rename-status-conflict"))
        );
    }
}
