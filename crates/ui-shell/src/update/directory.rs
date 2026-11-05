use super::*;
use crate::messages::DirectoryMessage;
use crate::update::metadata_hydration::schedule_visible_metadata_hydration;
use std::collections::HashSet;

pub fn handle(state: &mut AppState, message: DirectoryMessage) -> Task<Message> {
    match message {
        DirectoryMessage::RefreshRequested { pane_idx, path } => {
            return schedule_directory_refresh(state, pane_idx, path);
        }
        DirectoryMessage::Polled { path, entries } => {
            if state.pane(0).current_path != path {
                return Task::none();
            }

            let old_signature = directory_signature(&state.pane(0).entries);
            let new_signature = directory_signature(&entries);

            if old_signature != new_signature {
                let previous_entries = state.pane(0).entries.clone();
                let selected_paths: HashSet<String> = state
                    .pane(0)
                    .selected_indices
                    .iter()
                    .filter_map(|&i| state.pane(0).entry(i))
                    .map(|e| e.object_ref.display_path.clone())
                    .collect();
                let focused_path = state
                    .pane(0)
                    .focused_index
                    .and_then(|i| state.pane(0).entry(i))
                    .map(|e| e.object_ref.display_path.clone());

                invalidate_changed_thumbnail_entries(state, &previous_entries, &entries);
                state.pane_mut(0).replace_entries(entries);
                state.pane_mut(0).load_state = PaneLoadState::Loaded;
                prune_thumbnail_cache(state);
                sync_thumbnail_generation_queue(state);

                let pane = state.pane_mut(0);
                pane.selected_indices = selected_paths
                    .iter()
                    .filter_map(|path| {
                        pane.entry_index_by_display_path(path)
                            .filter(|index| pane.visible_position(*index).is_some())
                    })
                    .collect();

                pane.focused_index = focused_path
                    .as_deref()
                    .and_then(|path| pane.entry_index_by_display_path(path))
                    .filter(|index| pane.visible_position(*index).is_some())
                    .or_else(|| pane.first_visible_index());

                let thumbnail_task = generate_thumbnails(state);
                let preview_task = if state.active_pane == 0 {
                    refresh_preview_for_selection(state)
                } else {
                    Task::none()
                };
                let metadata_task = schedule_visible_metadata_hydration(state, 0);
                return Task::batch([thumbnail_task, preview_task, metadata_task]);
            }

            state.pane_mut(0).load_state = PaneLoadState::Loaded;
            return schedule_visible_metadata_hydration(state, 0);
        }
        DirectoryMessage::LoadFailed { path, error } => {
            if state.pane(0).current_path == path {
                state.pane_mut(0).load_state = PaneLoadState::Failed { message: error };
            }
        }
        DirectoryMessage::MountedVolumesChanged(volumes) => {
            app_runtime().sync_mounted_backends(&volumes);
            connections::sync_mounted_volume_group(state, volumes);
        }
        DirectoryMessage::CloudBackendsLoaded(items) => {
            connections::cloud_backends_loaded(state, items);
        }
        DirectoryMessage::RestoredJobsLoaded(restored_jobs) => {
            for restored in restored_jobs {
                if state.jobs.iter().any(|job| job.id == restored.id) {
                    continue;
                }
                state.jobs.push(JobDisplayInfo {
                    id: restored.id,
                    description: restored.description,
                    state: restored.state.clone(),
                    progress: ProgressInfo {
                        bytes_completed: 0,
                        bytes_total: None,
                        files_completed: 0,
                        files_total: Some(restored.files_total),
                    },
                    speed_samples: std::collections::VecDeque::new(),
                    speed_bps: None,
                });
            }
        }
    }

    Task::none()
}

fn schedule_directory_refresh(state: &AppState, pane_idx: usize, path: String) -> Task<Message> {
    if pane_idx >= 2 || state.active_dialog.is_some() || state.pending_operation.is_some() {
        return Task::none();
    }
    if state.pane(pane_idx).current_path != path {
        return Task::none();
    }

    let show_hidden = state.show_hidden_files;
    Task::perform(
        read_directory(path.clone(), show_hidden),
        move |result| match result {
            Ok(entries) if pane_idx == 0 => DirectoryMessage::Polled { path, entries }.into(),
            Ok(entries) => Message::Pane(pane_idx, PaneMessage::DirectoryLoaded { path, entries }),
            Err(error) if pane_idx == 0 => DirectoryMessage::LoadFailed { path, error }.into(),
            Err(error) => Message::Pane(pane_idx, PaneMessage::DirectoryLoadFailed { path, error }),
        },
    )
}
