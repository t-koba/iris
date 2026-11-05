use crate::i18n::tr;
use crate::messages::{Message, RemoteTransportMessage};
use crate::runtime::app_runtime;
use crate::state::{AppState, NotificationSeverity};
use iced::Task;

pub fn handle(state: &mut AppState, message: RemoteTransportMessage) -> Task<Message> {
    match message {
        RemoteTransportMessage::Redetect => redetect(state),
        RemoteTransportMessage::RedetectCompleted(detected) => redetect_completed(state, detected),
        RemoteTransportMessage::AddNew => {
            add_new(state);
            Task::none()
        }
        RemoteTransportMessage::Remove(id) => remove(state, id),
        RemoteTransportMessage::ToggleEnabled(id) => toggle_enabled(state, id),
        RemoteTransportMessage::BeginEdit(id) => {
            begin_edit(state, id);
            Task::none()
        }
        RemoteTransportMessage::CancelEdit => {
            cancel_edit(state);
            Task::none()
        }
        RemoteTransportMessage::SaveEdit => save_edit(state),
        RemoteTransportMessage::SecretDeleted(result) => secret_deleted(state, result),
        RemoteTransportMessage::EditField(field, value) => {
            edit_field(state, field, value);
            Task::none()
        }
        RemoteTransportMessage::EditTransport(kind) => {
            edit_transport(state, kind);
            Task::none()
        }
        RemoteTransportMessage::EditAuth(auth) => {
            edit_auth(state, auth);
            Task::none()
        }
    }
}

pub fn redetect(_state: &mut AppState) -> Task<Message> {
    Task::perform(
        async {
            tokio::task::spawn_blocking(crate::remote_transport::seed_from_mount_table)
                .await
                .unwrap_or_default()
        },
        |detected| RemoteTransportMessage::RedetectCompleted(detected).into(),
    )
}

pub fn redetect_completed(
    state: &mut AppState,
    detected: Vec<crate::remote_transport::RemoteTransportMapping>,
) -> Task<Message> {
    let added = crate::remote_transport::MappingTable::merge_auto_detected(
        &mut state.remote_transport_mappings,
        detected,
    );
    tracing::info!("remote transport: {} mapping(s) auto-detected", added);
    app_runtime().set_remote_transport_mappings(state.remote_transport_mappings.clone());
    Task::done(Message::SaveLayoutConfig)
}

pub fn add_new(state: &mut AppState) {
    state.remote_transport_edit_buffer =
        Some(crate::remote_transport::RemoteTransportMapping::new_user_defined());
}

pub fn remove(state: &mut AppState, id: String) -> Task<Message> {
    let deleted_mapping = state
        .remote_transport_mappings
        .iter()
        .find(|mapping| mapping.id == id)
        .cloned();
    state
        .remote_transport_mappings
        .retain(|mapping| mapping.id != id);
    app_runtime().set_remote_transport_mappings(state.remote_transport_mappings.clone());
    let save = Task::done(Message::SaveLayoutConfig);
    let Some(mapping) = deleted_mapping else {
        return save;
    };

    Task::batch([
        save,
        Task::perform(
            crate::config::delete_remote_transport_secret(mapping.id),
            |result| RemoteTransportMessage::SecretDeleted(result).into(),
        ),
    ])
}

pub fn toggle_enabled(state: &mut AppState, id: String) -> Task<Message> {
    if let Some(mapping) = state
        .remote_transport_mappings
        .iter_mut()
        .find(|mapping| mapping.id == id)
    {
        if !mapping.enabled {
            if let Err(error) = mapping.validate() {
                tracing::warn!(
                    "remote transfer validation failed before enabling: {}",
                    error
                );
                state.remote_transport_edit_buffer = Some(mapping.clone());
                return Task::none();
            }
        }

        mapping.enabled = !mapping.enabled;
        app_runtime().set_remote_transport_mappings(state.remote_transport_mappings.clone());
        return Task::done(Message::SaveLayoutConfig);
    }

    Task::none()
}

pub fn begin_edit(state: &mut AppState, id: String) {
    if let Some(mapping) = state
        .remote_transport_mappings
        .iter()
        .find(|mapping| mapping.id == id)
        .cloned()
    {
        state.remote_transport_edit_buffer = Some(mapping);
    }
}

pub fn cancel_edit(state: &mut AppState) {
    state.remote_transport_edit_buffer = None;
}

pub fn save_edit(state: &mut AppState) -> Task<Message> {
    if let Some(buffer) = state.remote_transport_edit_buffer.take() {
        if let Err(error) = buffer.validate() {
            tracing::warn!(
                "remote transfer mapping validation failed before save: {}",
                error
            );
            state.remote_transport_edit_buffer = Some(buffer);
            return Task::none();
        }

        if let Some(existing) = state
            .remote_transport_mappings
            .iter_mut()
            .find(|mapping| mapping.id == buffer.id)
        {
            *existing = buffer;
        } else {
            state.remote_transport_mappings.push(buffer);
        }

        app_runtime().set_remote_transport_mappings(state.remote_transport_mappings.clone());
        return Task::done(Message::SaveLayoutConfig);
    }

    Task::none()
}

pub fn edit_field(state: &mut AppState, field: crate::remote_transport::EditField, value: String) {
    if let Some(buffer) = state.remote_transport_edit_buffer.as_mut() {
        field.apply(buffer, value);
    }
}

pub fn edit_transport(state: &mut AppState, kind: crate::remote_transport::RemoteTransportKind) {
    if let Some(buffer) = state.remote_transport_edit_buffer.as_mut() {
        buffer.transport = kind;
    }
}

pub fn edit_auth(state: &mut AppState, auth: crate::remote_transport::SshAuthKind) {
    if let Some(buffer) = state.remote_transport_edit_buffer.as_mut() {
        buffer.auth = auth;
    }
}

fn secret_deleted(state: &mut AppState, result: Result<(), String>) -> Task<Message> {
    if let Err(error) = result {
        state.add_notification(
            NotificationSeverity::Warning,
            tr("notification-remote-secret-delete-failed-title"),
            error,
            vec![],
        );
    }
    Task::none()
}
