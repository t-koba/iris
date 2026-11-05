use super::*;
use crate::messages::{ConflictMessage, DialogMessage};

pub fn handle_dialog(state: &mut AppState, message: DialogMessage) -> Task<Message> {
    match message {
        DialogMessage::Close => {
            state.remote_transport_edit_buffer = None;
            state.shortcut_recording_action = None;
            state.active_dialog = None;
        }
        DialogMessage::InputChanged(value) => {
            if let Some(DialogState::CreateFolder { input_value }) = &mut state.active_dialog {
                *input_value = value;
            }
        }
    }

    Task::none()
}

pub fn handle_conflict(state: &mut AppState, message: ConflictMessage) -> Task<Message> {
    match message {
        ConflictMessage::Skip
        | ConflictMessage::Rename
        | ConflictMessage::Overwrite
        | ConflictMessage::SkipAll
        | ConflictMessage::OverwriteAll => {
            let resolution = match message {
                ConflictMessage::Skip | ConflictMessage::SkipAll => ConflictResolution::Skip,
                ConflictMessage::Rename => ConflictResolution::Rename,
                ConflictMessage::Overwrite | ConflictMessage::OverwriteAll => {
                    ConflictResolution::Overwrite
                }
                ConflictMessage::ToggleRememberChoice => ConflictResolution::Fail,
            };

            if matches!(
                message,
                ConflictMessage::SkipAll | ConflictMessage::OverwriteAll
            ) {
                if let Some(pending) = &mut state.pending_operation {
                    pending.remember_resolution = Some(resolution.clone());
                }
            }

            let task = apply_conflict_resolution(state, resolution);
            state.active_dialog = None;
            return task;
        }
        ConflictMessage::ToggleRememberChoice => {
            if let Some(DialogState::Conflict {
                remember_choice, ..
            }) = &mut state.active_dialog
            {
                *remember_choice = !*remember_choice;
            }
        }
    }

    Task::none()
}
