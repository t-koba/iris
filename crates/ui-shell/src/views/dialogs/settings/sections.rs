use crate::config::{LocaleChoice, ThemeChoice};
use crate::i18n::tr;
use crate::messages::{Message, SettingsMessage};
use crate::state::{CacheEncryptionPolicy, PreviewPosition, PropertiesPosition, StorageUsage};
use crate::views::theme;
use iced::widget::{button, checkbox, column, container, pick_list, row, text, text_input};
use iced::{Element, Length};

pub(super) fn build_appearance_settings_section<'a>(
    theme_choice: ThemeChoice,
    locale_choice: LocaleChoice,
    preview_position: PreviewPosition,
    properties_position: PropertiesPosition,
    notification_timeout_input: &'a str,
) -> Element<'a, Message> {
    let theme_picker = pick_list(ThemeChoice::ALL.as_slice(), Some(theme_choice), |choice| {
        SettingsMessage::SetThemeChoice(choice).into()
    })
    .text_size(11)
    .padding(4);
    let locale_picker = pick_list(
        LocaleChoice::ALL.as_slice(),
        Some(locale_choice),
        |choice| SettingsMessage::SetLocaleChoice(choice).into(),
    )
    .text_size(11)
    .padding(4);
    let preview_position_picker = pick_list(
        PreviewPosition::ALL.as_slice(),
        Some(preview_position),
        Message::SetPreviewPosition,
    )
    .text_size(11)
    .padding(4);
    let properties_position_picker = pick_list(
        PropertiesPosition::ALL.as_slice(),
        Some(properties_position),
        Message::SetPropertiesPosition,
    )
    .text_size(11)
    .padding(4);

    container(
        column![
            row![text(tr("settings-theme")).size(12), theme_picker]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            text(tr("settings-theme-help"))
                .size(10)
                .style(theme::muted_text_style),
            iced::widget::rule::horizontal(1),
            row![text(tr("language")).size(12), locale_picker]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            text(tr("language-help"))
                .size(10)
                .style(theme::muted_text_style),
            iced::widget::rule::horizontal(1),
            row![
                text(tr("settings-notification-timeout")).size(12),
                text_input(
                    &tr("settings-notification-timeout-placeholder"),
                    notification_timeout_input
                )
                .on_input(|value| SettingsMessage::SetNotificationTimeoutInput(value).into())
                .padding([4, 8])
                .size(11)
                .width(Length::Fixed(72.0)),
                text(tr("settings-notification-timeout-unit"))
                    .size(11)
                    .style(theme::muted_text_style),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            text(tr("settings-notification-timeout-help"))
                .size(10)
                .style(theme::muted_text_style),
            iced::widget::rule::horizontal(1),
            row![
                text(tr("settings-preview-position")).size(12),
                preview_position_picker
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            text(tr("settings-preview-position-help"))
                .size(10)
                .style(theme::muted_text_style),
            iced::widget::rule::horizontal(1),
            row![
                text(tr("settings-properties-position")).size(12),
                properties_position_picker
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            text(tr("settings-properties-position-help"))
                .size(10)
                .style(theme::muted_text_style),
        ]
        .spacing(12),
    )
    .padding(16)
    .style(theme::dialog_card_style)
    .into()
}

pub(super) fn build_cache_settings_section<'a>(
    policy: CacheEncryptionPolicy,
    map_tile_fetch_enabled: bool,
    thumbnail_cache_limit_input: &'a str,
    memory_budget_limit_input: &'a str,
    storage_usage: Option<StorageUsage>,
) -> Element<'a, Message> {
    let policy_picker = pick_list(
        CacheEncryptionPolicy::ALL.as_slice(),
        Some(policy),
        |policy| SettingsMessage::SetCacheEncryptionPolicy(policy).into(),
    )
    .text_size(11)
    .padding(4);
    let description = match policy {
        CacheEncryptionPolicy::Never => tr("settings-cache-encryption-never"),
        CacheEncryptionPolicy::Always => tr("settings-cache-encryption-always"),
        CacheEncryptionPolicy::Auto => tr("settings-cache-encryption-auto"),
    };
    let thumbnail_cache_limit_state =
        crate::state::parse_thumbnail_cache_limit_input_mib(thumbnail_cache_limit_input);
    let thumbnail_cache_limit_summary = thumbnail_cache_limit_state
        .map(|bytes| format_cache_size(bytes as u64))
        .unwrap_or_else(|| tr("settings-thumbnail-memory-limit-invalid"));
    let memory_budget_limit_state =
        crate::state::parse_memory_budget_limit_input_mib(memory_budget_limit_input);
    let memory_budget_limit_summary = memory_budget_limit_state
        .map(|bytes| format_cache_size(bytes as u64))
        .unwrap_or_else(|| tr("settings-memory-budget-limit-invalid"));
    let cache_size_text = match &storage_usage {
        Some(usage) => format_cache_size(usage.total_bytes()),
        None => tr("settings-cache-calculating"),
    };
    let usage_rows = storage_usage
        .as_ref()
        .map(storage_usage_rows)
        .unwrap_or_else(|| column![text(tr("settings-cache-calculating")).size(11)].spacing(4));

    container(
        column![
            row![
                text(tr("settings-cache-encryption")).size(12),
                policy_picker
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            text(description).size(10).style(theme::muted_text_style),
            iced::widget::rule::horizontal(1),
            row![
                text(tr("settings-thumbnail-memory-limit")).size(12),
                text_input(
                    &tr("settings-thumbnail-memory-limit-placeholder"),
                    thumbnail_cache_limit_input
                )
                .on_input(|value| SettingsMessage::SetThumbnailCacheLimitInput(value).into())
                .padding([4, 8])
                .size(11)
                .width(Length::Fixed(96.0)),
                text(tr("settings-thumbnail-memory-limit-unit"))
                    .size(11)
                    .style(theme::muted_text_style),
                text(thumbnail_cache_limit_summary)
                    .size(11)
                    .style(theme::muted_text_style),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            text(tr("settings-thumbnail-memory-limit-help"))
                .size(10)
                .style(theme::muted_text_style),
            iced::widget::rule::horizontal(1),
            row![
                text(tr("settings-memory-budget-limit")).size(12),
                text_input(
                    &tr("settings-memory-budget-limit-placeholder"),
                    memory_budget_limit_input
                )
                .on_input(|value| SettingsMessage::SetMemoryBudgetLimitInput(value).into())
                .padding([4, 8])
                .size(11)
                .width(Length::Fixed(96.0)),
                text(tr("settings-memory-budget-limit-unit"))
                    .size(11)
                    .style(theme::muted_text_style),
                text(memory_budget_limit_summary)
                    .size(11)
                    .style(theme::muted_text_style),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            text(tr("settings-memory-budget-limit-help"))
                .size(10)
                .style(theme::muted_text_style),
            iced::widget::rule::horizontal(1),
            checkbox(map_tile_fetch_enabled)
                .label(tr("settings-map-tile-fetch-enabled"))
                .on_toggle(|enabled| SettingsMessage::SetMapTileFetchEnabled(enabled).into())
                .size(14)
                .text_size(12),
            text(tr("settings-map-tile-fetch-help"))
                .size(10)
                .style(theme::muted_text_style),
            iced::widget::rule::horizontal(1),
            row![
                text(tr("settings-cache-usage")).size(12),
                text(cache_size_text).size(12),
                iced::widget::space::horizontal(),
                button(text(tr("settings-cache-delete")).size(11))
                    .on_press(SettingsMessage::ClearCache.into())
                    .padding([4, 12]),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            usage_rows,
            text(tr("settings-cache-delete-help"))
                .size(10)
                .style(theme::muted_text_style),
        ]
        .spacing(12),
    )
    .padding(16)
    .style(theme::dialog_card_style)
    .into()
}

fn storage_usage_rows<'a>(usage: &StorageUsage) -> iced::widget::Column<'a, Message> {
    column![
        storage_usage_row("settings-cache-category-thumbnails", usage.thumbnail_bytes),
        storage_usage_row("settings-cache-category-index", usage.index_bytes),
        storage_usage_row("settings-cache-category-tiles", usage.tile_bytes),
        storage_usage_row("settings-cache-category-logs", usage.log_bytes),
        storage_usage_row(
            "settings-cache-category-quarantine",
            usage.quarantined_bytes
        ),
    ]
    .spacing(4)
}

fn storage_usage_row<'a>(label_key: &str, bytes: u64) -> Element<'a, Message> {
    row![
        text(tr(label_key)).size(11).width(Length::Fill),
        text(format_cache_size(bytes)).size(11),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .into()
}

pub(super) fn build_shortcuts_settings_section<'a>(
    shortcut_overrides_json_input: &'a str,
    shortcut_overrides_error: Option<&'a str>,
    shortcut_recording_action: Option<&'a str>,
) -> Element<'a, Message> {
    let overrides =
        serde_json::from_str::<Vec<crate::config::ShortcutOverride>>(shortcut_overrides_json_input)
            .unwrap_or_default();
    let assignments =
        crate::input::keyboard::shortcut_assignments(&overrides).unwrap_or_else(|_| {
            crate::input::keyboard::default_shortcuts()
                .iter()
                .map(|shortcut| crate::input::keyboard::ShortcutAssignment {
                    action: shortcut.action,
                    combos: shortcut.combo.to_string(),
                })
                .collect()
        });
    let mut shortcuts = column![].spacing(4);
    for shortcut in assignments {
        let is_recording = shortcut_recording_action == Some(shortcut.action);
        let has_override = overrides
            .iter()
            .any(|override_entry| override_entry.action == shortcut.action);
        let action = shortcut.action.to_string();
        let clear_button = if has_override {
            button(text(tr("settings-shortcuts-clear")).size(10))
                .on_press(SettingsMessage::ClearShortcutOverride(action.clone()).into())
                .padding([2, 8])
        } else {
            button(text(tr("settings-shortcuts-clear")).size(10)).padding([2, 8])
        };
        let record_label = if is_recording {
            tr("settings-shortcuts-recording")
        } else {
            tr("settings-shortcuts-record")
        };
        shortcuts = shortcuts.push(
            row![
                text(shortcut.action).size(11).width(Length::FillPortion(2)),
                text(shortcut.combos)
                    .size(11)
                    .style(theme::muted_text_style)
                    .width(Length::FillPortion(3)),
                button(text(record_label).size(10))
                    .on_press(SettingsMessage::BeginShortcutRecording(action).into())
                    .padding([2, 8]),
                clear_button,
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
    }

    let error_text = shortcut_overrides_error
        .map(|error| {
            text(error.to_string())
                .size(10)
                .color(iced::Color::from_rgb8(220, 70, 70))
        })
        .unwrap_or_else(|| {
            text(tr("settings-shortcuts-json-help"))
                .size(10)
                .style(theme::muted_text_style)
        });

    container(
        column![
            text(tr("settings-shortcuts-current")).size(12),
            shortcuts,
            iced::widget::rule::horizontal(1),
            text(tr("settings-shortcuts-json")).size(12),
            text_input("[]", shortcut_overrides_json_input)
                .on_input(|value| SettingsMessage::SetShortcutOverridesJsonInput(value).into())
                .padding([4, 8])
                .size(11)
                .width(Length::Fill),
            error_text,
        ]
        .spacing(12),
    )
    .padding(16)
    .style(theme::dialog_card_style)
    .into()
}

pub(super) fn build_diagnostics_settings_section<'a>() -> Element<'a, Message> {
    container(
        column![
            text(tr("settings-diagnostics-logs")).size(12),
            text(tr("settings-diagnostics-logs-help"))
                .size(10)
                .style(theme::muted_text_style),
            button(text(tr("settings-diagnostics-open-log-folder")).size(11))
                .on_press(SettingsMessage::OpenLogFolder.into())
                .padding([4, 12]),
        ]
        .spacing(10),
    )
    .padding(16)
    .style(theme::dialog_card_style)
    .into()
}

fn format_cache_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
