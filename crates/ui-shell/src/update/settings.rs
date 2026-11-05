use super::helpers::{calculate_storage_usage, clear_disk_cache};
use super::thumbnail::{
    clear_thumbnail_cache, trim_thumbnail_cache_to_limits, trim_thumbnail_cache_to_memory_budget,
};
use crate::config::{LocaleChoice, ThemeChoice};
use crate::i18n::tr;
use crate::messages::{Message, SettingsMessage};
use crate::runtime::app_runtime;
use crate::state::{
    memory_budget_limit_input_from_bytes, notification_timeout_input_from_secs,
    parse_notification_timeout_input_secs, thumbnail_cache_limit_input_from_bytes, AppState,
    CacheEncryptionPolicy, DialogState, NotificationSeverity, SettingsSection,
};
use iced::Task;

pub fn handle(state: &mut AppState, message: SettingsMessage) -> Task<Message> {
    match message {
        SettingsMessage::Open => open_settings_dialog(state, SettingsSection::Appearance),
        SettingsMessage::SelectSection(section) => {
            if let Some(DialogState::Settings {
                selected_section,
                storage_usage,
                ..
            }) = &mut state.active_dialog
            {
                *selected_section = section;
                if section == SettingsSection::Cache && storage_usage.is_none() {
                    return Task::perform(async { calculate_storage_usage().await }, |usage| {
                        SettingsMessage::StorageUsageCalculated(usage).into()
                    });
                }
            }
            Task::none()
        }
        SettingsMessage::SetThemeChoice(choice) => set_theme_choice(state, choice),
        SettingsMessage::SetLocaleChoice(choice) => set_locale_choice(state, choice),
        SettingsMessage::SetNotificationTimeoutInput(value) => {
            set_notification_timeout_input(state, value)
        }
        SettingsMessage::SetShortcutOverridesJsonInput(value) => {
            set_shortcut_overrides_json_input(state, value)
        }
        SettingsMessage::BeginShortcutRecording(action) => {
            state.shortcut_recording_action = Some(action);
            Task::none()
        }
        SettingsMessage::CancelShortcutRecording => {
            state.shortcut_recording_action = None;
            Task::none()
        }
        SettingsMessage::ClearShortcutOverride(action) => clear_shortcut_override(state, &action),
        SettingsMessage::RecordedShortcut(combo) => record_shortcut_override(state, combo),
        SettingsMessage::SetRemoteCopyUndoEnabled(enabled) => {
            set_remote_copy_undo_enabled(state, enabled)
        }
        SettingsMessage::SetCacheEncryptionPolicy(policy) => {
            set_cache_encryption_policy(state, policy)
        }
        SettingsMessage::SetMapTileFetchEnabled(enabled) => {
            set_map_tile_fetch_enabled(state, enabled)
        }
        SettingsMessage::SetThumbnailCacheLimitInput(value) => {
            set_thumbnail_cache_limit_input(state, value)
        }
        SettingsMessage::SetMemoryBudgetLimitInput(value) => {
            set_memory_budget_limit_input(state, value)
        }
        SettingsMessage::StorageUsageCalculated(usage) => {
            storage_usage_calculated(state, usage);
            Task::none()
        }
        SettingsMessage::ClearCache => clear_cache(state),
        SettingsMessage::CacheClearCompleted(result) => cache_clear_completed(state, result),
        SettingsMessage::OpenLogFolder => open_log_folder(),
        SettingsMessage::OpenLogFolderCompleted(result) => open_log_folder_completed(state, result),
    }
}

pub fn open_settings_dialog(state: &mut AppState, section: SettingsSection) -> Task<Message> {
    if let Some(DialogState::Settings {
        selected_section, ..
    }) = &mut state.active_dialog
    {
        *selected_section = section;
    } else {
        state.active_dialog = Some(DialogState::Settings {
            selected_section: section,
            cache_encryption_policy: state.cache_encryption_policy,
            map_tile_fetch_enabled: state.map_tile_fetch_enabled,
            theme_choice: state.theme_choice,
            locale_choice: LocaleChoice::from_locale(state.locale.as_deref()),
            notification_timeout_input: notification_timeout_input_from_secs(
                state.notification_timeout_secs,
            ),
            shortcut_overrides_json_input: shortcut_overrides_json(&state.shortcut_overrides),
            shortcut_overrides_error: None,
            remote_copy_undo_enabled: state.remote_copy_undo_enabled,
            preview_position: state.preview_position,
            properties_position: state.properties_position,
            thumbnail_cache_limit_input: thumbnail_cache_limit_input_from_bytes(
                state.thumbnail_cache_limit_bytes,
            ),
            memory_budget_limit_input: memory_budget_limit_input_from_bytes(
                state.memory_budget_limit_bytes,
            ),
            storage_usage: None,
            network_form: crate::state::NetworkConnectFormState::default(),
            cloud_form: Box::new(crate::state::CloudConnectFormState::default()),
        });
    }

    Task::perform(async { calculate_storage_usage().await }, |usage| {
        SettingsMessage::StorageUsageCalculated(usage).into()
    })
}

fn shortcut_overrides_json(overrides: &[crate::config::ShortcutOverride]) -> String {
    serde_json::to_string(overrides).unwrap_or_else(|_| "[]".to_string())
}

pub fn set_theme_choice(state: &mut AppState, choice: ThemeChoice) -> Task<Message> {
    state.theme_choice = choice;
    if let Some(DialogState::Settings { theme_choice, .. }) = &mut state.active_dialog {
        *theme_choice = choice;
    }
    let save = Task::done(Message::SaveLayoutConfig);
    if choice == ThemeChoice::System {
        Task::batch([save, iced::system::theme().map(Message::SystemThemeChanged)])
    } else {
        save
    }
}

pub fn set_map_tile_fetch_enabled(state: &mut AppState, enabled: bool) -> Task<Message> {
    state.map_tile_fetch_enabled = enabled;
    if let Some(DialogState::Settings {
        map_tile_fetch_enabled,
        ..
    }) = &mut state.active_dialog
    {
        *map_tile_fetch_enabled = enabled;
    }
    Task::perform(async {}, |_| Message::SaveLayoutConfig)
}

pub fn set_shortcut_overrides_json_input(state: &mut AppState, value: String) -> Task<Message> {
    if let Some(DialogState::Settings {
        shortcut_overrides_json_input,
        shortcut_overrides_error,
        ..
    }) = &mut state.active_dialog
    {
        *shortcut_overrides_json_input = value.clone();
        match serde_json::from_str::<Vec<crate::config::ShortcutOverride>>(&value) {
            Ok(overrides) => {
                if let Err(error) = crate::input::keyboard::validate_shortcut_overrides(&overrides)
                {
                    *shortcut_overrides_error = Some(error);
                    return Task::none();
                }
                *shortcut_overrides_error = None;
                state.shortcut_overrides = overrides;
                Task::perform(async {}, |_| Message::SaveLayoutConfig)
            }
            Err(error) => {
                *shortcut_overrides_error = Some(error.to_string());
                Task::none()
            }
        }
    } else {
        Task::none()
    }
}

fn clear_shortcut_override(state: &mut AppState, action: &str) -> Task<Message> {
    let mut overrides = state.shortcut_overrides.clone();
    overrides.retain(|entry| entry.action != action);
    apply_shortcut_overrides(state, overrides)
}

fn record_shortcut_override(state: &mut AppState, combo: String) -> Task<Message> {
    let Some(action) = state.shortcut_recording_action.take() else {
        return Task::none();
    };
    let mut overrides = state.shortcut_overrides.clone();
    overrides.retain(|entry| entry.action != action);
    overrides.push(crate::config::ShortcutOverride { action, combo });
    apply_shortcut_overrides(state, overrides)
}

fn apply_shortcut_overrides(
    state: &mut AppState,
    overrides: Vec<crate::config::ShortcutOverride>,
) -> Task<Message> {
    if let Err(error) = crate::input::keyboard::validate_shortcut_overrides(&overrides) {
        if let Some(DialogState::Settings {
            shortcut_overrides_error,
            ..
        }) = &mut state.active_dialog
        {
            *shortcut_overrides_error = Some(error);
        }
        return Task::none();
    }

    let value = shortcut_overrides_json(&overrides);
    state.shortcut_overrides = overrides;
    if let Some(DialogState::Settings {
        shortcut_overrides_json_input,
        shortcut_overrides_error,
        ..
    }) = &mut state.active_dialog
    {
        *shortcut_overrides_json_input = value;
        *shortcut_overrides_error = None;
    }
    Task::perform(async {}, |_| Message::SaveLayoutConfig)
}

pub fn set_remote_copy_undo_enabled(state: &mut AppState, enabled: bool) -> Task<Message> {
    state.remote_copy_undo_enabled = enabled;
    if let Some(DialogState::Settings {
        remote_copy_undo_enabled,
        ..
    }) = &mut state.active_dialog
    {
        *remote_copy_undo_enabled = enabled;
    }
    Task::perform(async {}, |_| Message::SaveLayoutConfig)
}

pub fn set_locale_choice(state: &mut AppState, choice: LocaleChoice) -> Task<Message> {
    state.locale = choice.to_locale();
    crate::i18n::initialize(state.locale.as_deref());
    if let Some(DialogState::Settings { locale_choice, .. }) = &mut state.active_dialog {
        *locale_choice = choice;
    }
    Task::perform(async {}, |_| Message::SaveLayoutConfig)
}

pub fn set_notification_timeout_input(state: &mut AppState, value: String) -> Task<Message> {
    if let Some(DialogState::Settings {
        notification_timeout_input,
        ..
    }) = &mut state.active_dialog
    {
        *notification_timeout_input = value.clone();
    }

    let Some(timeout_secs) = parse_notification_timeout_input_secs(&value) else {
        return Task::none();
    };

    if state.notification_timeout_secs == timeout_secs {
        return Task::none();
    }

    state.notification_timeout_secs = timeout_secs;
    state.refresh_notification_expirations();
    Task::perform(async {}, |_| Message::SaveLayoutConfig)
}

pub fn set_cache_encryption_policy(
    state: &mut AppState,
    policy: CacheEncryptionPolicy,
) -> Task<Message> {
    state.cache_encryption_policy = policy;
    app_runtime().set_cache_encryption_policy(policy);
    if let Some(DialogState::Settings {
        cache_encryption_policy,
        ..
    }) = &mut state.active_dialog
    {
        *cache_encryption_policy = policy;
    }
    Task::perform(async {}, |_| Message::SaveLayoutConfig)
}

pub fn set_thumbnail_cache_limit_input(state: &mut AppState, value: String) -> Task<Message> {
    if let Some(DialogState::Settings {
        thumbnail_cache_limit_input,
        ..
    }) = &mut state.active_dialog
    {
        *thumbnail_cache_limit_input = value.clone();
    }

    let Some(limit_bytes) = crate::state::parse_thumbnail_cache_limit_input_mib(&value) else {
        return Task::none();
    };

    if state.thumbnail_cache_limit_bytes == limit_bytes {
        return Task::none();
    }

    state.thumbnail_cache_limit_bytes = limit_bytes;
    trim_thumbnail_cache_to_limits(state);
    Task::perform(async {}, |_| Message::SaveLayoutConfig)
}

pub fn set_memory_budget_limit_input(state: &mut AppState, value: String) -> Task<Message> {
    if let Some(DialogState::Settings {
        memory_budget_limit_input,
        ..
    }) = &mut state.active_dialog
    {
        *memory_budget_limit_input = value.clone();
    }

    let Some(limit_bytes) = crate::state::parse_memory_budget_limit_input_mib(&value) else {
        return Task::none();
    };
    if state.memory_budget_limit_bytes == limit_bytes {
        return Task::none();
    }

    state.memory_budget_limit_bytes = limit_bytes;
    iris_core_domain::memory_budget::MemoryBudget::global().set_cap_bytes(limit_bytes);
    trim_thumbnail_cache_to_memory_budget(state);
    Task::perform(async {}, |_| Message::SaveLayoutConfig)
}

pub fn storage_usage_calculated(state: &mut AppState, usage: crate::state::StorageUsage) {
    if let Some(DialogState::Settings { storage_usage, .. }) = &mut state.active_dialog {
        *storage_usage = Some(usage);
    }
}

pub fn clear_cache(state: &mut AppState) -> Task<Message> {
    clear_thumbnail_cache(state);
    Task::perform(async { clear_disk_cache().await }, |result| {
        SettingsMessage::CacheClearCompleted(result).into()
    })
}

pub fn cache_clear_completed(state: &mut AppState, result: Result<(), String>) -> Task<Message> {
    match result {
        Ok(()) => {
            state.add_notification(
                NotificationSeverity::Info,
                tr("notification-cache-clear-completed-title"),
                tr("notification-cache-clear-completed-body"),
                vec![],
            );
            Task::perform(async { calculate_storage_usage().await }, |usage| {
                SettingsMessage::StorageUsageCalculated(usage).into()
            })
        }
        Err(error) => {
            state.add_notification(
                NotificationSeverity::Error,
                tr("notification-cache-clear-failed-title"),
                error,
                vec![],
            );
            Task::none()
        }
    }
}

fn open_log_folder() -> Task<Message> {
    Task::perform(
        async {
            let dir = crate::observability::log_dir()
                .ok_or_else(|| tr("notification-open-log-folder-unavailable-body"))?;
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|error| error.to_string())?;
            open::that(dir).map_err(|error| error.to_string())
        },
        |result| SettingsMessage::OpenLogFolderCompleted(result).into(),
    )
}

fn open_log_folder_completed(state: &mut AppState, result: Result<(), String>) -> Task<Message> {
    if let Err(error) = result {
        state.add_notification(
            NotificationSeverity::Error,
            tr("notification-open-log-folder-failed-title"),
            error,
            vec![],
        );
    }

    Task::none()
}
