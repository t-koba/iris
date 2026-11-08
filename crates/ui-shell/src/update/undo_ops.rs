use super::file_ops::start_pending_file_operation_with_undo_action;
use crate::i18n::tr;
use crate::messages::Message;
use crate::runtime::app_runtime;
use crate::state::{AppState, PendingFileItem, PendingUndoAction};
use crate::undo::{UndoItem, UndoOpKind, UndoableOp};
use fluent_bundle::FluentArgs;
use iced::Task;
use iris_core_domain::types::object::ObjectRef;
use std::path::{Path, PathBuf};

pub(crate) fn record_undo_op(state: &mut AppState, kind: UndoOpKind, items: Vec<UndoItem>) {
    if items.is_empty() {
        return;
    }
    state.undo_journal.record(UndoableOp {
        kind,
        items,
        performed_at: std::time::SystemTime::now(),
    });
}

pub(crate) fn record_undo_op_task(kind: UndoOpKind, items: Vec<UndoItem>) -> Task<Message> {
    if items.is_empty() {
        return Task::none();
    }
    Task::perform(enrich_undo_items(kind.clone(), items), move |items| {
        Message::RecordUndoOp { kind, items }
    })
}

pub(crate) fn perform_undo(state: &mut AppState, op: UndoableOp) -> Task<Message> {
    match op.kind {
        UndoOpKind::Move => {
            if let Err(error) = validate_undo_op(&op, UndoValidationTarget::Destination) {
                return undo_failure_task(op, error);
            }
            let items = undo_transfer_items(&op);
            if items.is_empty() {
                return undo_failure_task(op, "nothing to undo".to_string());
            }
            start_pending_file_operation_with_undo_action(
                state,
                items,
                true,
                Some(PendingUndoAction::PushRedo(op)),
            )
        }
        UndoOpKind::Copy => {
            if let Err(error) = validate_undo_op(&op, UndoValidationTarget::Destination) {
                return undo_failure_task(op, error);
            }
            let op_for_task = op.clone();
            Task::perform(
                async move {
                    delete_paths(
                        op_for_task
                            .items
                            .iter()
                            .map(|item| item.destination_path.clone())
                            .collect(),
                        true,
                    )
                    .await
                },
                move |result| Message::UndoCompleted {
                    op: op.clone(),
                    redo_op: Some(op),
                    result,
                },
            )
        }
        UndoOpKind::Rename => {
            if let Err(error) = validate_undo_op(&op, UndoValidationTarget::Destination) {
                return undo_failure_task(op, error);
            }
            let Some(item) = op.items.first().cloned() else {
                return undo_failure_task(op, "nothing to undo".to_string());
            };
            Task::perform(
                async move { rename_path(item.destination_path, item.source_path).await },
                move |result| Message::UndoCompleted {
                    op: op.clone(),
                    redo_op: Some(op),
                    result,
                },
            )
        }
        UndoOpKind::CreateFolder => {
            if let Err(error) = validate_undo_op(&op, UndoValidationTarget::Destination) {
                return undo_failure_task(op, error);
            }
            let paths = op
                .items
                .iter()
                .map(|item| item.destination_path.clone())
                .collect();
            Task::perform(
                async move { delete_empty_directories(paths).await },
                move |result| Message::UndoCompleted {
                    op: op.clone(),
                    redo_op: Some(op),
                    result,
                },
            )
        }
        UndoOpKind::TrashDelete => {
            if let Some(existing) = op.items.iter().find(|item| item.source_path.exists()) {
                let error = format!("{} already exists", existing.source_path.display());
                return undo_failure_task(op, error);
            }
            let receipts = op
                .items
                .iter()
                .map(|item| iris_platform_integration::trash::TrashReceipt {
                    original_path: item.source_path.clone(),
                    trashed_path: item.trashed_path.clone(),
                    trash_id: item.trash_id.clone(),
                })
                .collect();
            Task::perform(
                async move { restore_trash_receipts(receipts).await },
                move |result| Message::UndoCompleted {
                    op: op.clone(),
                    redo_op: Some(op),
                    result,
                },
            )
        }
    }
}

pub(crate) fn perform_redo(state: &mut AppState, op: UndoableOp) -> Task<Message> {
    match op.kind {
        UndoOpKind::Copy | UndoOpKind::Move => {
            if let Err(error) = validate_undo_op(&op, UndoValidationTarget::Source) {
                return redo_failure_task(op, error);
            }
            let items = redo_transfer_items(&op);
            if items.is_empty() {
                return redo_failure_task(op, "nothing to redo".to_string());
            }
            let is_move = matches!(op.kind, UndoOpKind::Move);
            start_pending_file_operation_with_undo_action(
                state,
                items,
                is_move,
                Some(PendingUndoAction::RecordOp(op)),
            )
        }
        UndoOpKind::Rename => {
            if let Err(error) = validate_undo_op(&op, UndoValidationTarget::Source) {
                return redo_failure_task(op, error);
            }
            let Some(item) = op.items.first().cloned() else {
                return redo_failure_task(op, "nothing to redo".to_string());
            };
            Task::perform(
                async move { rename_path(item.source_path, item.destination_path).await },
                move |result| Message::RedoCompleted {
                    op: op.clone(),
                    undo_op: Some(op),
                    result,
                },
            )
        }
        UndoOpKind::CreateFolder => {
            let Some(item) = op.items.first().cloned() else {
                return redo_failure_task(op, "nothing to redo".to_string());
            };
            if item.destination_path.exists() {
                return redo_failure_task(
                    op,
                    format!("{} already exists", item.destination_path.display()),
                );
            }
            Task::perform(
                async move { create_folder_path(item.destination_path).await },
                move |result| Message::RedoCompleted {
                    op: op.clone(),
                    undo_op: Some(op),
                    result,
                },
            )
        }
        UndoOpKind::TrashDelete => {
            if let Err(error) = validate_undo_op(&op, UndoValidationTarget::Source) {
                return redo_failure_task(op, error);
            }
            let paths = op
                .items
                .iter()
                .map(|item| item.source_path.clone())
                .collect();
            Task::perform(trash_paths_for_undo(paths), move |result| {
                trash_redo_completed(op, result)
            })
        }
    }
}

fn trash_redo_completed(op: UndoableOp, result: Result<Vec<UndoItem>, String>) -> Message {
    match result {
        Ok(items) => Message::RedoCompleted {
            undo_op: Some(UndoableOp {
                items,
                ..op.clone()
            }),
            op,
            result: Ok(()),
        },
        Err(error) => Message::RedoCompleted {
            op,
            undo_op: None,
            result: Err(error),
        },
    }
}

#[derive(Clone, Copy)]
enum UndoValidationTarget {
    Source,
    Destination,
}

async fn enrich_undo_items(kind: UndoOpKind, items: Vec<UndoItem>) -> Vec<UndoItem> {
    let mut enriched = Vec::with_capacity(items.len());
    for item in items {
        enriched.push(enrich_undo_item_async(kind.clone(), item).await);
    }
    enriched
}

async fn enrich_undo_item_async(kind: UndoOpKind, mut item: UndoItem) -> UndoItem {
    let path = match kind {
        UndoOpKind::Copy | UndoOpKind::Move | UndoOpKind::Rename | UndoOpKind::CreateFolder => {
            &item.destination_path
        }
        UndoOpKind::TrashDelete => &item.source_path,
    };
    if let Ok(metadata) = tokio::fs::metadata(path).await {
        item.size = Some(metadata.len());
        item.mtime = metadata.modified().ok();
    }
    item
}

pub(crate) fn undo_requires_remote_confirmation(op: &UndoableOp) -> bool {
    matches!(op.kind, UndoOpKind::Copy)
        && op.items.iter().any(|item| {
            let destination = item.destination_path.to_string_lossy();
            app_runtime().is_virtual_display_path(&destination)
                || !app_runtime()
                    .backend_id_for_path(&item.destination_path)
                    .0
                    .starts_with("local")
        })
}

fn validate_undo_op(op: &UndoableOp, target: UndoValidationTarget) -> Result<(), String> {
    for item in &op.items {
        let path = match target {
            UndoValidationTarget::Source => &item.source_path,
            UndoValidationTarget::Destination => &item.destination_path,
        };
        validate_undo_item_path(path, item)?;
    }
    Ok(())
}

fn validate_undo_item_path(path: &Path, item: &UndoItem) -> Result<(), String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("{} is no longer valid: {error}", path.display()))?;
    if let Some(expected_size) = item.size {
        if metadata.len() != expected_size {
            return Err(format!("{} changed since the operation", path.display()));
        }
    }
    if let Some(expected_mtime) = item.mtime {
        if metadata.modified().ok() != Some(expected_mtime) {
            return Err(format!("{} changed since the operation", path.display()));
        }
    }
    Ok(())
}

fn undo_transfer_items(op: &UndoableOp) -> Vec<PendingFileItem> {
    op.items
        .iter()
        .map(|item| PendingFileItem {
            source: pending_object_ref(&item.destination_path),
            destination_path: item.source_path.to_string_lossy().to_string(),
        })
        .collect()
}

fn redo_transfer_items(op: &UndoableOp) -> Vec<PendingFileItem> {
    op.items
        .iter()
        .map(|item| PendingFileItem {
            source: pending_object_ref(&item.source_path),
            destination_path: item.destination_path.to_string_lossy().to_string(),
        })
        .collect()
}

fn pending_object_ref(path: &Path) -> ObjectRef {
    ObjectRef {
        key: iris_core_domain::types::object::ObjectKey {
            backend_id: app_runtime().backend_id_for_path(path),
            stable_id: iris_core_domain::types::object::StableId(format!(
                "pending:{}",
                path.to_string_lossy()
            )),
        },
        parent_stable_id: None,
        display_path: path.to_string_lossy().to_string(),
    }
}

fn undo_failure_task(op: UndoableOp, error: String) -> Task<Message> {
    Task::done(Message::UndoCompleted {
        op,
        redo_op: None,
        result: Err(error),
    })
}

fn redo_failure_task(op: UndoableOp, error: String) -> Task<Message> {
    Task::done(Message::RedoCompleted {
        op,
        undo_op: None,
        result: Err(error),
    })
}

async fn rename_path(from: PathBuf, to: PathBuf) -> Result<(), String> {
    let target = pending_object_ref(&from);
    let Some(new_name) = to.file_name().and_then(|name| name.to_str()) else {
        return Err(format!("{} has no file name", to.display()));
    };
    app_runtime().rename_object(&target, new_name).await
}

async fn create_folder_path(path: PathBuf) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err(format!("{} has no parent", path.display()));
    };
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(format!("{} has no file name", path.display()));
    };
    app_runtime()
        .create_folder(&parent.to_string_lossy(), name)
        .await
}

async fn delete_paths(paths: Vec<PathBuf>, prefer_trash: bool) -> Result<(), String> {
    let mut refs = Vec::with_capacity(paths.len());
    for path in paths {
        refs.push(pending_object_ref(&path));
    }
    app_runtime().delete_objects(&refs, prefer_trash).await
}

async fn delete_empty_directories(paths: Vec<PathBuf>) -> Result<(), String> {
    for path in paths {
        let path_str = path.to_string_lossy().to_string();
        if app_runtime().is_virtual_display_path(&path_str) {
            if !app_runtime()
                .read_directory_entries(&path_str, true, Some(1))
                .await?
                .is_empty()
            {
                return Err(format!("{} is not empty", path.display()));
            }
            delete_paths(vec![path], false).await?;
        } else {
            let mut entries = tokio::fs::read_dir(&path)
                .await
                .map_err(|error| error.to_string())?;
            if entries
                .next_entry()
                .await
                .map_err(|error| error.to_string())?
                .is_some()
            {
                return Err(format!("{} is not empty", path.display()));
            }
            tokio::fs::remove_dir(&path)
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

async fn restore_trash_receipts(
    receipts: Vec<iris_platform_integration::trash::TrashReceipt>,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        iris_platform_integration::trash::restore_files_with_receipts(&receipts)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

pub(crate) async fn trash_paths_for_undo(paths: Vec<PathBuf>) -> Result<Vec<UndoItem>, String> {
    let mut metadata = Vec::with_capacity(paths.len());
    for path in &paths {
        metadata.push(
            tokio::fs::metadata(path)
                .await
                .map_err(|error| format!("{} is no longer valid: {error}", path.display()))?,
        );
    }
    let receipts = tokio::task::spawn_blocking(move || {
        iris_platform_integration::trash::trash_files_with_receipts(&paths)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;
    let items = receipts
        .into_iter()
        .zip(metadata)
        .map(|(receipt, metadata)| UndoItem {
            destination_path: receipt.original_path.clone(),
            source_path: receipt.original_path,
            size: Some(metadata.len()),
            mtime: metadata.modified().ok(),
            trashed_path: receipt.trashed_path,
            trash_id: receipt.trash_id,
        })
        .collect();
    Ok(items)
}

pub(crate) fn undo_success_message(op: &UndoableOp) -> String {
    undo_redo_success_message("notification-undo-completed-body", op)
}

pub(crate) fn redo_success_message(op: &UndoableOp) -> String {
    undo_redo_success_message("notification-redo-completed-body", op)
}

fn undo_redo_success_message(key: &str, op: &UndoableOp) -> String {
    let mut args = FluentArgs::new();
    args.set("count", op.items.len() as i64);
    args.set("operation", tr(undo_operation_key(&op.kind)));
    crate::i18n::tr_args(key, Some(&args))
}

fn undo_operation_key(kind: &UndoOpKind) -> &'static str {
    match kind {
        UndoOpKind::Copy => "undo-operation-copy",
        UndoOpKind::Move => "undo-operation-move",
        UndoOpKind::Rename => "undo-operation-rename",
        UndoOpKind::CreateFolder => "undo-operation-create-folder",
        UndoOpKind::TrashDelete => "undo-operation-trash-delete",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // Receipt injection keeps this regression deterministic and avoids mutating OS/user Trash.
    #[test]
    fn trash_redo_passes_fresh_receipts_to_next_undo() {
        let old_items: Vec<_> = ["first.txt", "second.txt"]
            .into_iter()
            .map(|name| UndoItem {
                source_path: PathBuf::from(name),
                destination_path: PathBuf::from(name),
                size: Some(7),
                mtime: Some(std::time::SystemTime::UNIX_EPOCH),
                trashed_path: Some(PathBuf::from("trash").join(name)),
                trash_id: Some(name.into()),
            })
            .collect();
        let op = UndoableOp {
            kind: UndoOpKind::TrashDelete,
            items: old_items.clone(),
            performed_at: std::time::SystemTime::UNIX_EPOCH,
        };
        let mut journal = crate::undo::UndoJournal::default();
        journal.record(op.clone());
        // Model the first undo, then a redo whose old IDs are now occupied by other items.
        let undone_op = journal.pop_undo().unwrap();
        journal.push_redo(undone_op);
        let fresh_items: Vec<_> = old_items
            .iter()
            .cloned()
            .map(|mut item| {
                let name = format!("{}.2", item.source_path.display());
                item.trashed_path = Some(PathBuf::from("trash").join(&name));
                item.trash_id = Some(name.into());
                item
            })
            .collect();
        let message = trash_redo_completed(journal.pop_redo().unwrap(), Ok(fresh_items.clone()));
        let Message::RedoCompleted {
            op: completed_op,
            undo_op,
            result,
        } = message
        else {
            panic!("expected redo completion");
        };
        assert_eq!(completed_op, op);
        assert_eq!(result, Ok(()));
        // This is the operation the RedoCompleted handler pushes, not the original operation.
        journal.push_undo_from_redo(undo_op.unwrap());
        let next_undo = journal.pop_undo().unwrap();
        assert_eq!(next_undo.items, fresh_items);
        assert_ne!(next_undo.items, old_items);
        assert_eq!(next_undo.kind, op.kind);
        assert_eq!(next_undo.performed_at, op.performed_at);

        let Message::RedoCompleted {
            op: failed_op,
            undo_op,
            result,
        } = trash_redo_completed(next_undo.clone(), Err("delete failed".into()))
        else {
            panic!("expected redo completion");
        };
        assert_eq!(failed_op, next_undo);
        assert!(undo_op.is_none());
        assert_eq!(result, Err("delete failed".into()));
    }

    #[tokio::test]
    async fn undo_metadata_enrichment_and_validation_rejects_changes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("target.txt");
        fs::write(&path, "before").unwrap();

        let mut state = AppState::default();
        let items = enrich_undo_items(
            UndoOpKind::Copy,
            vec![UndoItem {
                source_path: dir.path().join("source.txt"),
                destination_path: path.clone(),
                size: None,
                mtime: None,
                trashed_path: None,
                trash_id: None,
            }],
        )
        .await;
        record_undo_op(&mut state, UndoOpKind::Copy, items);

        let op = state.undo_journal.pop_undo().unwrap();
        assert_eq!(op.items[0].size, Some(6));
        assert!(validate_undo_op(&op, UndoValidationTarget::Destination).is_ok());

        fs::write(&path, "after-change").unwrap();

        assert!(validate_undo_op(&op, UndoValidationTarget::Destination).is_err());
    }

    #[tokio::test]
    async fn rename_path_round_trips_local_file() {
        let dir = TempDir::new().unwrap();
        let original = dir.path().join("old.txt");
        let renamed = dir.path().join("new.txt");
        fs::write(&original, "content").unwrap();

        rename_path(original.clone(), renamed.clone())
            .await
            .unwrap();
        assert!(!original.exists());
        assert_eq!(fs::read_to_string(&renamed).unwrap(), "content");

        rename_path(renamed.clone(), original.clone())
            .await
            .unwrap();
        assert!(original.exists());
        assert!(!renamed.exists());
    }

    #[tokio::test]
    async fn create_folder_undo_removes_only_empty_local_folder() {
        let dir = TempDir::new().unwrap();
        let folder = dir.path().join("created");

        create_folder_path(folder.clone()).await.unwrap();
        assert!(folder.is_dir());
        delete_empty_directories(vec![folder.clone()])
            .await
            .unwrap();
        assert!(!folder.exists());

        create_folder_path(folder.clone()).await.unwrap();
        fs::write(folder.join("child.txt"), "content").unwrap();
        assert!(delete_empty_directories(vec![folder.clone()])
            .await
            .is_err());
        assert!(folder.exists());
    }
}
