use super::*;
use crate::messages::SidebarMessage;
use crate::state::{SidebarDropTarget, SidebarGroup, SidebarGroupKind, SidebarItem};

fn save_layout_config() -> Task<Message> {
    Task::done(Message::SaveLayoutConfig)
}

pub fn handle(state: &mut AppState, message: SidebarMessage) -> Task<Message> {
    match message {
        SidebarMessage::ToggleVisibility => {
            state.sidebar_visible = !state.sidebar_visible;
            state.responsive_sidebar_dismissed = false;
            return save_layout_config();
        }
        SidebarMessage::ToggleGroupCollapsed(group_idx) => {
            if let Some(group) = state.sidebar_groups.get_mut(group_idx) {
                group.collapsed = !group.collapsed;
            }
            return save_layout_config();
        }
        SidebarMessage::AddCurrentPathToGroup(group_idx) => {
            let path = state.active_pane().current_path.clone();
            if !path.is_empty() {
                if let Some(group) = state.sidebar_groups.get_mut(group_idx) {
                    let already = group.items.iter().any(|item| item.path == path);
                    if !already {
                        let name = std::path::Path::new(&path)
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.clone());
                        group.items.push(SidebarItem {
                            name,
                            path,
                            badge: "*".to_string(),
                            badge_color: [0.5, 0.5, 0.5, 1.0],
                            ejectable: false,
                        });
                        return save_layout_config();
                    }
                }
            }
        }
        SidebarMessage::AddCurrentPathToFavorites => {
            let path = state.active_pane().current_path.clone();
            if !path.is_empty() {
                let index = match state
                    .sidebar_groups
                    .iter()
                    .position(|group| group.kind == SidebarGroupKind::Favorites)
                {
                    Some(index) => index,
                    None => {
                        state
                            .sidebar_groups
                            .push(SidebarGroup::new(SidebarGroupKind::Favorites));
                        state.sidebar_groups.len() - 1
                    }
                };
                let already = state.sidebar_groups[index]
                    .items
                    .iter()
                    .any(|item| item.path == path);
                if !already {
                    let name = std::path::Path::new(&path)
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.clone());
                    state.sidebar_groups[index].items.push(SidebarItem {
                        name,
                        path,
                        badge: "*".to_string(),
                        badge_color: [0.5, 0.5, 0.5, 1.0],
                        ejectable: false,
                    });
                    return save_layout_config();
                }
            }
        }
        SidebarMessage::RemoveItem { group, item } => {
            if let Some(sidebar_group) = state.sidebar_groups.get_mut(group) {
                if item < sidebar_group.items.len() {
                    sidebar_group.items.remove(item);
                    return save_layout_config();
                }
            }
        }
        SidebarMessage::AddGroup => {
            state
                .sidebar_groups
                .push(SidebarGroup::new(SidebarGroupKind::Custom));
            return save_layout_config();
        }
        SidebarMessage::BeginRenameGroup(group_idx) => {
            if let Some(group) = state
                .sidebar_groups
                .get(group_idx)
                .filter(|group| !group.is_system())
            {
                state.sidebar_rename_input = group.name.clone();
                state.sidebar_renaming_group = Some(group_idx);
            }
        }
        SidebarMessage::ConfirmRenameGroup(new_name) => {
            if let Some(index) = state.sidebar_renaming_group.take() {
                if !new_name.trim().is_empty() {
                    if let Some(group) = state.sidebar_groups.get_mut(index) {
                        group.name = new_name.trim().to_string();
                    }
                }
                state.sidebar_rename_input.clear();
                return save_layout_config();
            }
        }
        SidebarMessage::CancelRenameGroup => {
            state.sidebar_renaming_group = None;
            state.sidebar_rename_input.clear();
        }
        SidebarMessage::RenameInput(value) => {
            state.sidebar_rename_input = value;
        }
        SidebarMessage::ToggleJobs => {
            state.sidebar_jobs_visible = !state.sidebar_jobs_visible;
        }
        SidebarMessage::DragItem(group, item) => {
            if state
                .sidebar_groups
                .get(group)
                .map(|sidebar_group| !sidebar_group.is_system())
                .unwrap_or(false)
            {
                state.sidebar_dragging_item = Some((group, item));
                state.sidebar_dragging_group = None;
                state.sidebar_drop_target = None;
            }
        }
        SidebarMessage::DragGroup(group_idx) => {
            if state
                .sidebar_groups
                .get(group_idx)
                .map(|sidebar_group| !sidebar_group.is_system())
                .unwrap_or(false)
            {
                state.sidebar_dragging_group = Some(group_idx);
                state.sidebar_dragging_item = None;
                state.sidebar_drop_target = None;
            }
        }
        SidebarMessage::DragOver(target) => {
            state.sidebar_drop_target = target;
        }
        SidebarMessage::Drop => {
            let save = if let Some((from_group, from_item)) = state.sidebar_dragging_item.take() {
                if let Some(target) = state.sidebar_drop_target.take() {
                    match target {
                        SidebarDropTarget::ItemPosition {
                            group: to_group,
                            position: to_pos,
                        } => {
                            let target_ok = state
                                .sidebar_groups
                                .get(to_group)
                                .map(|group| !group.is_system())
                                .unwrap_or(false);
                            if target_ok {
                                if from_group == to_group {
                                    if let Some(group) = state.sidebar_groups.get_mut(from_group) {
                                        if from_item < group.items.len() {
                                            let item = group.items.remove(from_item);
                                            let insert_at = to_pos.min(group.items.len());
                                            group.items.insert(insert_at, item);
                                        }
                                    }
                                } else {
                                    let moved_item = state
                                        .sidebar_groups
                                        .get_mut(from_group)
                                        .and_then(|group| {
                                            if from_item < group.items.len() {
                                                Some(group.items.remove(from_item))
                                            } else {
                                                None
                                            }
                                        });
                                    if let Some(item) = moved_item {
                                        if let Some(group) = state.sidebar_groups.get_mut(to_group)
                                        {
                                            let insert_at = to_pos.min(group.items.len());
                                            group.items.insert(insert_at, item);
                                        }
                                    }
                                }
                                true
                            } else {
                                false
                            }
                        }
                        SidebarDropTarget::BeforeGroup(_) => false,
                    }
                } else {
                    false
                }
            } else if let Some(from_group) = state.sidebar_dragging_group.take() {
                if let Some(SidebarDropTarget::BeforeGroup(to_group)) =
                    state.sidebar_drop_target.take()
                {
                    if from_group != to_group && from_group < state.sidebar_groups.len() {
                        let group = state.sidebar_groups.remove(from_group);
                        let insert_at = if to_group > from_group {
                            (to_group - 1).min(state.sidebar_groups.len())
                        } else {
                            to_group.min(state.sidebar_groups.len())
                        };
                        state.sidebar_groups.insert(insert_at, group);
                    }
                    true
                } else {
                    false
                }
            } else {
                state.sidebar_drop_target = None;
                false
            };

            if save {
                return save_layout_config();
            }
        }
        SidebarMessage::DragCancel => {
            state.sidebar_dragging_item = None;
            state.sidebar_dragging_group = None;
            state.sidebar_drop_target = None;
        }
        SidebarMessage::OpenInNewWindow(path) => {
            let spawned = std::env::current_exe()
                .and_then(|exe| std::process::Command::new(exe).arg(&path).spawn());
            if let Err(error) = spawned {
                state.add_notification(
                    crate::state::NotificationSeverity::Error,
                    crate::i18n::tr("notification-open-new-window-failed-title"),
                    error.to_string(),
                    vec![crate::state::NotificationAction::Dismiss],
                );
            }
        }
    }

    Task::none()
}
