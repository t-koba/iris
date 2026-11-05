use super::*;
use crate::i18n::tr;

pub fn current_dnd_operation(state: &AppState) -> Option<DndOperation> {
    let target = state.dnd_state.target()?;
    let target_dir = dnd_target_directory(state, target)?;

    match &state.dnd_state {
        DndState::Dragging {
            source_pane,
            source_indices,
            ..
        } => {
            let sources: Vec<ObjectRef> = source_indices
                .iter()
                .filter_map(|&index| state.pane(*source_pane).entry(index))
                .map(|entry| entry.object_ref.clone())
                .collect();

            if sources.is_empty() {
                None
            } else {
                Some(app_runtime().dnd_operation_for_drop(
                    &sources,
                    &target_dir,
                    state.keyboard_modifiers,
                ))
            }
        }
        DndState::ExternalDragging { source_paths, .. } => {
            if source_paths.is_empty() {
                None
            } else {
                Some(app_runtime().dnd_operation_for_paths(
                    source_paths,
                    &target_dir,
                    state.keyboard_modifiers,
                ))
            }
        }
        DndState::Pressed { .. } | DndState::Idle => None,
    }
}

pub fn perform_dnd_drop(state: &mut AppState) -> Task<Message> {
    let (source_pane, source_indices, target) = match &state.dnd_state {
        DndState::Dragging {
            source_pane,
            source_indices,
            target: Some(target),
        } => (*source_pane, source_indices.clone(), *target),
        _ => {
            state.dnd_state = DndState::Idle;
            return Task::none();
        }
    };

    state.dnd_state = DndState::Idle;

    let Some(target_dir) = dnd_target_directory(state, target) else {
        return Task::none();
    };

    let source_entries: Vec<_> = source_indices
        .iter()
        .filter_map(|&index| state.pane(source_pane).entry(index).cloned())
        .collect();
    let source_refs: Vec<ObjectRef> = source_entries
        .iter()
        .map(|entry| entry.object_ref.clone())
        .collect();

    if source_refs.is_empty() {
        return Task::none();
    }

    let operation =
        app_runtime().dnd_operation_for_drop(&source_refs, &target_dir, state.keyboard_modifiers);

    if matches!(operation, DndOperation::Link) {
        state.add_notification(
            NotificationSeverity::Warning,
            tr("dnd-link-unsupported-title"),
            tr("dnd-link-unsupported-body"),
            vec![NotificationAction::Dismiss],
        );
        return Task::none();
    }

    let is_move = matches!(operation, DndOperation::Move);

    let mut items = Vec::new();
    let mut skipped = 0usize;

    for entry in source_entries {
        let source_path = PathBuf::from(&entry.object_ref.display_path);

        if is_move && source_path.parent() == Some(target_dir.as_path()) {
            skipped += 1;
            continue;
        }

        if entry.is_dir && target_dir.starts_with(&source_path) {
            skipped += 1;
            continue;
        }

        items.push(PendingFileItem {
            source: entry.object_ref.clone(),
            destination_path: target_dir.join(&entry.name).to_string_lossy().to_string(),
        });
    }

    if skipped > 0 {
        state.add_notification(
            NotificationSeverity::Warning,
            tr("dnd-skipped-items-title"),
            tr("dnd-skipped-items-body"),
            vec![NotificationAction::Dismiss],
        );
    }

    start_pending_file_operation(state, items, is_move)
}

pub fn perform_external_drop(
    state: &mut AppState,
    source_paths: Vec<PathBuf>,
    target: Option<DndTarget>,
) -> Task<Message> {
    let target = target.unwrap_or(DndTarget::PaneDirectory {
        pane: state.active_pane,
    });
    let Some(target_dir) = dnd_target_directory(state, target) else {
        return Task::none();
    };

    if source_paths.is_empty() {
        return Task::none();
    }

    let operation =
        app_runtime().dnd_operation_for_paths(&source_paths, &target_dir, state.keyboard_modifiers);

    if matches!(operation, DndOperation::Link) {
        state.add_notification(
            NotificationSeverity::Warning,
            tr("dnd-link-unsupported-title"),
            tr("dnd-link-unsupported-body"),
            vec![NotificationAction::Dismiss],
        );
        return Task::none();
    }

    let is_move = matches!(operation, DndOperation::Move);
    let mut items = Vec::new();
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for source_path in source_paths {
        let file_name = file_name_from_path(&source_path.to_string_lossy());

        if is_move && source_path.parent() == Some(target_dir.as_path()) {
            skipped += 1;
            continue;
        }

        let is_dir = source_path
            .metadata()
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false);

        if is_move && is_dir && target_dir.starts_with(&source_path) {
            skipped += 1;
            continue;
        }

        let source = match app_runtime().object_ref_for_drop_path(&source_path) {
            Ok(source) => source,
            Err(_) => {
                failed += 1;
                continue;
            }
        };

        items.push(PendingFileItem {
            source,
            destination_path: target_dir.join(file_name).to_string_lossy().to_string(),
        });
    }

    if skipped > 0 {
        state.add_notification(
            NotificationSeverity::Warning,
            tr("dnd-skipped-items-title"),
            tr("dnd-skipped-items-body"),
            vec![NotificationAction::Dismiss],
        );
    }

    if failed > 0 {
        state.add_notification(
            NotificationSeverity::Warning,
            tr("dnd-failed-items-title"),
            tr("dnd-failed-items-body"),
            vec![NotificationAction::Dismiss],
        );
    }

    start_pending_file_operation(state, items, is_move)
}

fn dnd_target_directory(state: &AppState, target: DndTarget) -> Option<PathBuf> {
    match target {
        DndTarget::PaneDirectory { pane } => {
            let current_path = state.pane_checked(pane)?.current_path.trim();
            if current_path.is_empty() {
                None
            } else {
                Some(PathBuf::from(current_path))
            }
        }
        DndTarget::Entry { pane, index } => {
            let pane_state = state.pane_checked(pane)?;
            let entry = pane_state.entry(index)?;
            if entry.is_dir && !entry.is_inaccessible {
                Some(PathBuf::from(&pane_state.current_path).join(&entry.name))
            } else {
                None
            }
        }
    }
}
