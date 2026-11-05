use super::connections::build_connections_settings_section;
mod remote_transfer;
mod sections;

use remote_transfer::{build_remote_transport_edit_form, build_remote_transport_list};
use sections::{
    build_appearance_settings_section, build_cache_settings_section,
    build_diagnostics_settings_section, build_shortcuts_settings_section,
};

use crate::config::{LocaleChoice, ThemeChoice};
use crate::i18n::tr;
use crate::messages::{DialogMessage, Message, SettingsMessage};
use crate::remote_transport::RemoteTransportMapping;
use crate::state::{
    CacheEncryptionPolicy, CloudConnectFormState, NetworkConnectFormState, PreviewPosition,
    PropertiesPosition, SettingsSection, StorageUsage,
};
use crate::views::theme;
use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Element, Length};

pub struct SettingsDialogView<'a> {
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub selected_section: SettingsSection,
    pub policy: CacheEncryptionPolicy,
    pub map_tile_fetch_enabled: bool,
    pub theme_choice: ThemeChoice,
    pub locale_choice: LocaleChoice,
    pub notification_timeout_input: &'a str,
    pub shortcut_overrides_json_input: &'a str,
    pub shortcut_overrides_error: Option<&'a str>,
    pub shortcut_recording_action: Option<&'a str>,
    pub remote_copy_undo_enabled: bool,
    pub preview_position: PreviewPosition,
    pub properties_position: PropertiesPosition,
    pub thumbnail_cache_limit_input: &'a str,
    pub memory_budget_limit_input: &'a str,
    pub storage_usage: Option<StorageUsage>,
    pub network_form: &'a NetworkConnectFormState,
    pub cloud_form: &'a CloudConnectFormState,
    pub remote_transport_mappings: &'a [RemoteTransportMapping],
    pub remote_transport_edit_buffer: Option<&'a RemoteTransportMapping>,
}

pub fn view_settings_dialog<'a>(view: SettingsDialogView<'a>) -> Element<'a, Message> {
    let SettingsDialogView {
        viewport_width,
        viewport_height,
        selected_section,
        policy,
        map_tile_fetch_enabled,
        theme_choice,
        locale_choice,
        notification_timeout_input,
        shortcut_overrides_json_input,
        shortcut_overrides_error,
        shortcut_recording_action,
        remote_copy_undo_enabled,
        preview_position,
        properties_position,
        thumbnail_cache_limit_input,
        memory_budget_limit_input,
        storage_usage,
        network_form,
        cloud_form,
        remote_transport_mappings,
        remote_transport_edit_buffer,
    } = view;
    let (title, description, section_content): (String, String, Element<'a, Message>) =
        match selected_section {
            SettingsSection::Appearance => (
                tr("settings-section-appearance"),
                tr("settings-appearance-description"),
                build_appearance_settings_section(
                    theme_choice,
                    locale_choice,
                    preview_position,
                    properties_position,
                    notification_timeout_input,
                ),
            ),
            SettingsSection::Cache => (
                tr("settings-section-cache"),
                tr("settings-cache-description"),
                build_cache_settings_section(
                    policy,
                    map_tile_fetch_enabled,
                    thumbnail_cache_limit_input,
                    memory_budget_limit_input,
                    storage_usage,
                ),
            ),
            SettingsSection::Shortcuts => (
                tr("settings-section-shortcuts"),
                tr("settings-shortcuts-description"),
                build_shortcuts_settings_section(
                    shortcut_overrides_json_input,
                    shortcut_overrides_error,
                    shortcut_recording_action,
                ),
            ),
            SettingsSection::Diagnostics => (
                tr("settings-section-diagnostics"),
                tr("settings-diagnostics-description"),
                build_diagnostics_settings_section(),
            ),
            SettingsSection::Connections => (
                tr("settings-section-connections"),
                tr("settings-connections-description"),
                build_connections_settings_section(network_form, cloud_form),
            ),
            SettingsSection::RemoteTransfer => (
                tr("settings-section-remote-transfer"),
                tr("settings-remote-transfer-description"),
                if let Some(buffer) = remote_transport_edit_buffer {
                    build_remote_transport_edit_form(buffer)
                } else {
                    build_remote_transport_list(remote_transport_mappings, remote_copy_undo_enabled)
                },
            ),
        };

    let navigation = column![
        text(tr("settings-title")).size(16),
        text(tr("settings-category-workspace"))
            .size(10)
            .style(theme::muted_text_style),
        settings_nav_item(SettingsSection::Appearance, selected_section),
        settings_nav_item(SettingsSection::Cache, selected_section),
        settings_nav_item(SettingsSection::Shortcuts, selected_section),
        settings_nav_item(SettingsSection::Diagnostics, selected_section),
        iced::widget::Space::new().height(Length::Fixed(10.0)),
        text(tr("settings-category-connections"))
            .size(10)
            .style(theme::muted_text_style),
        settings_nav_item(SettingsSection::Connections, selected_section),
        settings_nav_item(SettingsSection::RemoteTransfer, selected_section),
    ]
    .spacing(6)
    .width(Length::Fixed(180.0));

    let details = column![
        text(title).size(18),
        text(description).size(11).style(theme::muted_text_style),
        iced::widget::rule::horizontal(1),
        scrollable(section_content).height(Length::Fill),
        row![
            iced::widget::space::horizontal(),
            button(text(tr("settings-close")).size(12))
                .on_press(DialogMessage::Close.into())
                .padding(8),
        ]
        .spacing(8),
    ]
    .spacing(12)
    .width(Length::Fill);

    let (dialog_width, dialog_height) = settings_dialog_size(viewport_width, viewport_height);
    let layout: Element<'_, Message> = if dialog_width >= 760.0 {
        row![navigation, iced::widget::rule::vertical(1), details]
            .spacing(16)
            .height(Length::Fill)
            .align_y(iced::Alignment::Start)
            .into()
    } else {
        let section_tabs = row![
            settings_nav_chip(SettingsSection::Appearance, selected_section),
            settings_nav_chip(SettingsSection::Cache, selected_section),
            settings_nav_chip(SettingsSection::Shortcuts, selected_section),
            settings_nav_chip(SettingsSection::Diagnostics, selected_section),
            settings_nav_chip(SettingsSection::Connections, selected_section),
            settings_nav_chip(SettingsSection::RemoteTransfer, selected_section),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center);
        column![
            text(tr("settings-title")).size(16),
            scrollable(section_tabs)
                .direction(iced::widget::scrollable::Direction::Horizontal(
                    iced::widget::scrollable::Scrollbar::default(),
                ))
                .height(Length::Shrink),
            iced::widget::rule::horizontal(1),
            details,
        ]
        .spacing(10)
        .height(Length::Fill)
        .into()
    };

    container(layout)
        .width(Length::Fixed(dialog_width))
        .height(Length::Fixed(dialog_height))
        .padding(theme::DIALOG_PADDING)
        .style(theme::dialog_card_style)
        .into()
}

fn settings_dialog_size(viewport_width: f32, viewport_height: f32) -> (f32, f32) {
    (
        (viewport_width - 24.0).clamp(320.0, 860.0),
        (viewport_height - 24.0).clamp(320.0, 620.0),
    )
}

fn settings_nav_chip<'a>(
    section: SettingsSection,
    selected_section: SettingsSection,
) -> Element<'a, Message> {
    button(text(settings_section_label(section)).size(11))
        .on_press(SettingsMessage::SelectSection(section).into())
        .padding([5, 8])
        .style(if section == selected_section {
            button::primary
        } else {
            button::text
        })
        .into()
}

fn settings_nav_item<'a>(
    section: SettingsSection,
    selected_section: SettingsSection,
) -> Element<'a, Message> {
    let label = button(text(settings_section_label(section)).size(13))
        .on_press(SettingsMessage::SelectSection(section).into())
        .width(Length::Fill)
        .padding([8, 10])
        .style(button::text);

    if section == selected_section {
        container(label)
            .width(Length::Fill)
            .style(theme::sidebar_active_style)
            .into()
    } else {
        label.into()
    }
}

fn settings_section_label(section: SettingsSection) -> String {
    tr(section.i18n_key())
}

#[cfg(test)]
mod tests {
    use super::settings_dialog_size;

    #[test]
    fn settings_dialog_stays_inside_supported_viewports() {
        for (width, height) in [
            (640.0, 480.0),
            (800.0, 600.0),
            (1_280.0, 800.0),
            (1_920.0, 1_080.0),
        ] {
            let (dialog_width, dialog_height) = settings_dialog_size(width, height);
            assert!(dialog_width <= width - 24.0);
            assert!(dialog_height <= height - 24.0);
        }
    }
}
