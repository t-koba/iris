use super::support::{
    sample_cloud_form, sample_network_form, sample_state, snapshot_view, state_with_cloud_dialog,
    state_with_conflict_dialog, state_with_create_folder_dialog, state_with_delete_dialog,
    state_with_error_dialog, state_with_network_dialog, state_with_path_bar_edit,
    state_with_settings_dialog,
};
use iris_ui_shell::state::{DialogState, PreviewData, SettingsSection};
use iris_ui_shell::views::{conflict_dialog, dialogs, notifications, preview, properties, sidebar};

#[test]
fn major_views_render_stable_snapshots() {
    let state = sample_state();
    let _ = iris_ui_shell::app::view_for_test(&state);
    let _ = sidebar::view_sidebar(&state);
    snapshot_view("view__sidebar_default", "sidebar", &state);

    let mut renaming_sidebar = sample_state();
    renaming_sidebar.sidebar_renaming_group = Some(1);
    renaming_sidebar.sidebar_rename_input = "Workspaces".into();
    let _ = sidebar::view_sidebar(&renaming_sidebar);
    snapshot_view("view__sidebar_renaming", "sidebar", &renaming_sidebar);

    let mut sidebar_jobs = sample_state();
    sidebar_jobs.sidebar_jobs_visible = true;
    let _ = sidebar::view_sidebar(&sidebar_jobs);
    snapshot_view("view__sidebar_jobs_visible", "sidebar", &sidebar_jobs);

    let _ = notifications::view_notifications(&state.notifications);
    snapshot_view("view__notifications_default", "notifications", &state);

    let _ = preview::view_preview_panel(&state);
    snapshot_view("view__preview_text", "preview_panel", &state);

    let mut preview_loading = sample_state();
    preview_loading.preview_loading = true;
    preview_loading.preview_data = None;
    let _ = preview::view_preview_panel(&preview_loading);
    snapshot_view("view__preview_loading", "preview_panel", &preview_loading);

    let mut preview_unknown = sample_state();
    preview_unknown.preview_data = Some(PreviewData::Unknown {
        file_type: "raw".into(),
        size: 8_388_608,
    });
    let _ = preview::view_preview_panel(&preview_unknown);
    snapshot_view("view__preview_unknown", "preview_panel", &preview_unknown);

    let _ = properties::view_properties_panel(&state);
    snapshot_view("view__properties_text", "properties_panel", &state);

    let mut multi_select = sample_state();
    multi_select.pane_mut(0).rebuild_visible_indices();
    multi_select.pane_mut(0).select_all();
    multi_select.pane_mut(0).focused_index = Some(0);
    let _ = properties::view_properties_panel(&multi_select);
    snapshot_view(
        "view__properties_multi_select",
        "properties_panel",
        &multi_select,
    );

    let delete_state = state_with_delete_dialog();
    if let Some(DialogState::DeleteConfirmation {
        target_names,
        use_trash,
        can_use_trash,
        ..
    }) = &delete_state.active_dialog
    {
        let _ = dialogs::view_delete_dialog(target_names, *use_trash, *can_use_trash);
    }
    snapshot_view("view__dialog_delete", "dialog_delete", &delete_state);

    let create_state = state_with_create_folder_dialog();
    if let Some(DialogState::CreateFolder { input_value }) = &create_state.active_dialog {
        let _ = dialogs::view_create_folder_dialog(input_value);
    }
    snapshot_view(
        "view__dialog_create_folder",
        "dialog_create_folder",
        &create_state,
    );

    let path_bar_state = state_with_path_bar_edit();
    snapshot_view("view__path_bar_edit", "path_bar_edit", &path_bar_state);

    let error_state = state_with_error_dialog();
    if let Some(DialogState::Error { title, message }) = &error_state.active_dialog {
        let _ = dialogs::view_error_dialog(title, message);
    }
    snapshot_view("view__dialog_error", "dialog_error", &error_state);

    let conflict_state = state_with_conflict_dialog();
    if let Some(DialogState::Conflict {
        source,
        destination_path,
        source_size,
        destination_size,
        source_mtime,
        destination_mtime,
        remember_choice,
    }) = &conflict_state.active_dialog
    {
        let _ = conflict_dialog::view_conflict_dialog(
            &source.display_path,
            destination_path,
            *source_size,
            *destination_size,
            *source_mtime,
            *destination_mtime,
            *remember_choice,
        );
    }
    snapshot_view("view__dialog_conflict", "dialog_conflict", &conflict_state);

    for (case, section) in [
        (
            "view__dialog_settings_appearance",
            SettingsSection::Appearance,
        ),
        ("view__dialog_settings_cache", SettingsSection::Cache),
        (
            "view__dialog_settings_connections",
            SettingsSection::Connections,
        ),
        (
            "view__dialog_settings_remote_transfer",
            SettingsSection::RemoteTransfer,
        ),
    ] {
        let state = state_with_settings_dialog(section);
        if let Some(DialogState::Settings {
            selected_section,
            cache_encryption_policy,
            map_tile_fetch_enabled,
            theme_choice,
            locale_choice,
            notification_timeout_input,
            shortcut_overrides_json_input,
            shortcut_overrides_error,
            remote_copy_undo_enabled,
            preview_position,
            properties_position,
            thumbnail_cache_limit_input,
            memory_budget_limit_input,
            storage_usage,
            network_form,
            cloud_form,
        }) = &state.active_dialog
        {
            let _ = dialogs::view_settings_dialog(dialogs::SettingsDialogView {
                viewport_width: state.window_width,
                viewport_height: state.window_height,
                selected_section: *selected_section,
                policy: *cache_encryption_policy,
                map_tile_fetch_enabled: *map_tile_fetch_enabled,
                theme_choice: *theme_choice,
                locale_choice: *locale_choice,
                notification_timeout_input,
                shortcut_overrides_json_input,
                shortcut_overrides_error: shortcut_overrides_error.as_deref(),
                shortcut_recording_action: None,
                remote_copy_undo_enabled: *remote_copy_undo_enabled,
                preview_position: *preview_position,
                properties_position: *properties_position,
                thumbnail_cache_limit_input,
                memory_budget_limit_input,
                storage_usage: storage_usage.clone(),
                network_form,
                cloud_form: cloud_form.as_ref(),
                remote_transport_mappings: &state.remote_transport_mappings,
                remote_transport_edit_buffer: state.remote_transport_edit_buffer.as_ref(),
            });
        }
        snapshot_view(case, "dialog_settings", &state);
    }

    let network_state = state_with_network_dialog();
    let _ = dialogs::view_connect_network_dialog(&sample_network_form());
    snapshot_view(
        "view__dialog_connect_network",
        "dialog_connect_network",
        &network_state,
    );

    let cloud_state = state_with_cloud_dialog();
    let _ = dialogs::view_connect_cloud_dialog(&sample_cloud_form());
    snapshot_view(
        "view__dialog_connect_cloud",
        "dialog_connect_cloud",
        &cloud_state,
    );
}

#[test]
fn responsive_app_view_constructs_for_supported_layout_matrix() {
    for (width, height) in [
        (640.0, 480.0),
        (800.0, 600.0),
        (1_280.0, 800.0),
        (1_920.0, 1_080.0),
    ] {
        for dual in [false, true] {
            for position in iris_ui_shell::state::PreviewPosition::ALL {
                let mut state = sample_state();
                state.window_width = width;
                state.window_height = height;
                state.dual_pane_enabled = dual;
                state.preview_position = position;
                state.sidebar_visible = true;
                state.preview_panel_visible = true;
                state.properties_panel_visible = true;

                let _ = iris_ui_shell::app::view_for_test(&state);
            }
        }
    }
}
