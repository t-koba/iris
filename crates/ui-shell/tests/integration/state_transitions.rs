use super::support::{
    sample_state, state_with_conflict_dialog, state_with_create_folder_dialog,
    state_with_delete_dialog, state_with_path_bar_edit, state_with_settings_dialog,
};
use iris_ui_shell::messages::{
    ConflictMessage, DialogMessage, Message, PaneMessage, SettingsMessage, SidebarMessage,
};
use iris_ui_shell::state::{
    PreviewPosition, PropertiesPosition, SettingsSection, SidebarDropTarget,
};

#[test]
fn primary_messages_keep_state_transitions_stable() {
    run_transition(
        "transition__sidebar_toggle_visibility",
        sample_state(),
        SidebarMessage::ToggleVisibility.into(),
    );

    run_transition(
        "transition__sidebar_toggle_jobs",
        sample_state(),
        SidebarMessage::ToggleJobs.into(),
    );

    run_transition(
        "transition__sidebar_add_group",
        sample_state(),
        SidebarMessage::AddGroup.into(),
    );

    run_transition(
        "transition__sidebar_begin_rename_group",
        sample_state(),
        SidebarMessage::BeginRenameGroup(1).into(),
    );

    let mut rename_input_state = sample_state();
    rename_input_state.sidebar_renaming_group = Some(1);
    run_transition(
        "transition__sidebar_rename_input",
        rename_input_state,
        SidebarMessage::RenameInput("Pinned".into()).into(),
    );

    let mut rename_confirm_state = sample_state();
    rename_confirm_state.sidebar_renaming_group = Some(1);
    rename_confirm_state.sidebar_rename_input = "Pinned".into();
    run_transition(
        "transition__sidebar_confirm_rename_group",
        rename_confirm_state,
        SidebarMessage::ConfirmRenameGroup("Pinned".into()).into(),
    );

    run_transition(
        "transition__sidebar_add_current_path_to_favorites",
        sample_state(),
        SidebarMessage::AddCurrentPathToFavorites.into(),
    );

    run_transition(
        "transition__sidebar_toggle_group_collapsed",
        sample_state(),
        SidebarMessage::ToggleGroupCollapsed(1).into(),
    );

    let drag_item_state = sample_state();
    run_transition(
        "transition__sidebar_drag_item",
        drag_item_state,
        SidebarMessage::DragItem(1, 0).into(),
    );

    let mut drag_over_state = sample_state();
    drag_over_state.sidebar_dragging_item = Some((1, 0));
    run_transition(
        "transition__sidebar_drag_over",
        drag_over_state,
        SidebarMessage::DragOver(Some(SidebarDropTarget::ItemPosition {
            group: 1,
            position: 1,
        }))
        .into(),
    );

    let mut drop_state = sample_state();
    drop_state.sidebar_dragging_item = Some((1, 0));
    drop_state.sidebar_drop_target = Some(SidebarDropTarget::ItemPosition {
        group: 1,
        position: 1,
    });
    run_transition(
        "transition__sidebar_drop",
        drop_state,
        SidebarMessage::Drop.into(),
    );

    run_transition(
        "transition__settings_open",
        sample_state(),
        SettingsMessage::Open.into(),
    );

    run_transition(
        "transition__settings_select_cache",
        state_with_settings_dialog(SettingsSection::Appearance),
        SettingsMessage::SelectSection(SettingsSection::Cache).into(),
    );

    run_transition(
        "transition__settings_set_cache_policy",
        state_with_settings_dialog(SettingsSection::Appearance),
        SettingsMessage::SetCacheEncryptionPolicy(
            iris_ui_shell::state::CacheEncryptionPolicy::Always,
        )
        .into(),
    );

    run_transition(
        "transition__settings_set_limit_input",
        state_with_settings_dialog(SettingsSection::Cache),
        SettingsMessage::SetThumbnailCacheLimitInput("64".into()).into(),
    );

    run_transition(
        "transition__settings_cache_size_calculated",
        state_with_settings_dialog(SettingsSection::Cache),
        SettingsMessage::StorageUsageCalculated(iris_ui_shell::state::StorageUsage {
            thumbnail_bytes: 16 * 1024 * 1024,
            ..iris_ui_shell::state::StorageUsage::default()
        })
        .into(),
    );

    run_transition(
        "transition__workspace_toggle_preview_panel",
        sample_state(),
        Message::TogglePreviewPanel,
    );

    run_transition(
        "transition__workspace_toggle_properties_panel",
        sample_state(),
        Message::TogglePropertiesPanel,
    );

    run_transition(
        "transition__workspace_set_preview_position",
        sample_state(),
        Message::SetPreviewPosition(PreviewPosition::Top),
    );

    run_transition(
        "transition__workspace_set_properties_position",
        sample_state(),
        Message::SetPropertiesPosition(PropertiesPosition::LeftOfPreview),
    );

    run_transition(
        "transition__workspace_toggle_dual_pane",
        sample_state(),
        Message::ToggleDualPane,
    );

    let mut set_active_state = sample_state();
    set_active_state.dual_pane_enabled = true;
    run_transition(
        "transition__workspace_set_active_pane",
        set_active_state,
        Message::SetActivePane(1),
    );

    run_transition(
        "transition__notifications_dismiss",
        sample_state(),
        Message::DismissNotification(2),
    );

    run_transition(
        "transition__dialog_create_folder_open",
        sample_state(),
        Message::CreateFolder,
    );

    run_transition(
        "transition__dialog_input_changed_create_folder",
        state_with_create_folder_dialog(),
        Message::Dialog(DialogMessage::InputChanged("Inbox".into())),
    );

    run_transition(
        "transition__path_bar_input_changed",
        state_with_path_bar_edit(),
        Message::Pane(0, PaneMessage::PathInputChanged("~/Archive".into())),
    );

    run_transition(
        "transition__dialog_close",
        state_with_create_folder_dialog(),
        Message::Dialog(DialogMessage::Close),
    );

    run_transition(
        "transition__conflict_toggle_remember",
        state_with_conflict_dialog(),
        Message::Conflict(ConflictMessage::ToggleRememberChoice),
    );

    run_transition(
        "transition__delete_toggle_use_trash",
        state_with_delete_dialog(),
        Message::SetDeleteUseTrash(false),
    );

    run_transition(
        "transition__window_resized",
        sample_state(),
        Message::WindowResized(iced::Size::new(1440.0, 900.0)),
    );

    run_transition(
        "transition__modifiers_changed",
        sample_state(),
        Message::ModifiersChanged(iced::keyboard::Modifiers::SHIFT),
    );
}

fn run_transition(case: &str, mut state: iris_ui_shell::state::AppState, message: Message) {
    let before = super::support::snapshot_state(&state);
    let _ = iris_ui_shell::update::update(&mut state, message.clone());
    super::support::assert_json_snapshot(
        case,
        &super::support::TransitionSnapshot {
            case: case.to_string(),
            message: format!("{message:?}"),
            before,
            after: super::support::snapshot_state(&state),
        },
    );
}
