use super::*;
use fluent_bundle::FluentArgs;

fn restore_window_task(config: &LayoutConfig) -> Task<Message> {
    let width = config.window_width.clamp(640.0, 7680.0);
    let height = config.window_height.clamp(480.0, 4320.0);
    let position = config
        .window_x
        .zip(config.window_y)
        .filter(|(x, y)| x.is_finite() && y.is_finite())
        .map(|(x, y)| iced::Point::new(x.clamp(-20_000.0, 20_000.0), y.clamp(-20_000.0, 20_000.0)));

    window::latest().then(move |window_id| {
        let Some(window_id) = window_id else {
            return Task::none();
        };
        let resize = window::resize(window_id, iced::Size::new(width, height));
        if let Some(position) = position {
            Task::batch([resize, window::move_to(window_id, position)])
        } else {
            resize
        }
    })
}

fn save_layout_config_now(state: &mut AppState) -> Task<Message> {
    state.last_config_save = Some(std::time::Instant::now());
    let config = LayoutConfig {
        sidebar_width: state.sidebar_width,
        right_panel_width: state.right_panel_width,
        preview_panel_height: state.preview_panel_height,
        window_width: state.window_width,
        window_height: state.window_height,
        window_x: state.window_x,
        window_y: state.window_y,
        preview_position: state.preview_position,
        dual_pane_enabled: state.dual_pane_enabled,
        pane_split_ratio: state.pane_split_ratio,
        sidebar_visible: state.sidebar_visible,
        preview_panel_visible: state.preview_panel_visible,
        properties_panel_visible: state.properties_panel_visible,
        show_hidden_files: state.show_hidden_files,
        cache_encryption_policy: state.cache_encryption_policy,
        thumbnail_cache_limit_bytes: state.thumbnail_cache_limit_bytes,
        memory_budget_limit_bytes: state.memory_budget_limit_bytes,
        map_tile_fetch_enabled: state.map_tile_fetch_enabled,
        theme: state.theme_choice,
        locale: state.locale.clone(),
        shortcut_overrides: state.shortcut_overrides.clone(),
        remote_copy_undo_enabled: state.remote_copy_undo_enabled,
        notification_timeout_secs: state.notification_timeout_secs,
        list_columns: state.pane(0).list_columns.clone(),
        secondary_list_columns: state.pane(1).list_columns.clone(),
        properties_split_ratio: state.properties_split_ratio,
        properties_position: state.properties_position,
        tabs: state.pane_tab_configs(),
        sidebar_groups: state
            .sidebar_groups
            .iter()
            .map(|group| crate::config::SidebarGroupConfig {
                name: group.name.clone(),
                collapsed: group.collapsed,
                items: if group.is_system() {
                    Vec::new()
                } else {
                    group
                        .items
                        .iter()
                        .map(|item| crate::config::SidebarItemConfig {
                            name: item.name.clone(),
                            path: item.path.clone(),
                            badge: item.badge.clone(),
                            badge_color: item.badge_color,
                            ejectable: item.ejectable,
                        })
                        .collect()
                },
                system: group.is_system(),
                kind: Some(group.kind),
            })
            .collect(),
        remote_transport_mappings: state.remote_transport_mappings.clone(),
        remote_transport_secret_warnings: Vec::new(),
    };
    Task::perform(
        crate::config::save_config(config),
        Message::LayoutConfigSaved,
    )
}

fn close_quick_look(state: &mut AppState) -> Task<Message> {
    if !state.quick_look_open {
        return Task::none();
    }
    state.quick_look_open = false;

    if state.preview_panel_visible {
        return Task::none();
    }

    clear_preview_state(state);
    if state.properties_panel_visible {
        refresh_preview_for_selection(state)
    } else {
        Task::none()
    }
}

fn select_next_quick_look_item(state: &mut AppState) -> Task<Message> {
    if !state.quick_look_open {
        return Task::none();
    }
    state.active_pane_mut().focus_next();
    state.active_pane_mut().select_focused();
    refresh_preview_for_selection(state)
}

fn select_previous_quick_look_item(state: &mut AppState) -> Task<Message> {
    if !state.quick_look_open {
        return Task::none();
    }
    state.active_pane_mut().focus_previous();
    state.active_pane_mut().select_focused();
    refresh_preview_for_selection(state)
}

fn open_command_palette(state: &mut AppState) -> Task<Message> {
    state.command_palette_open = true;
    state.command_palette_query.clear();
    state.command_palette_selected = 0;
    iced::widget::operation::focus(crate::app::ids::command_palette_input_id())
}

fn close_command_palette(state: &mut AppState) {
    state.command_palette_open = false;
    state.command_palette_query.clear();
    state.command_palette_selected = 0;
}

fn move_command_palette_selection(state: &mut AppState, delta: isize) {
    let len = crate::commands::command_palette_results(state)
        .len()
        .min(crate::commands::COMMAND_PALETTE_VISIBLE_LIMIT);
    if len == 0 {
        state.command_palette_selected = 0;
        return;
    }

    let current = state.command_palette_selected.min(len - 1);
    state.command_palette_selected = if delta.is_negative() {
        current.checked_sub(delta.unsigned_abs()).unwrap_or(len - 1)
    } else {
        (current + delta as usize) % len
    };
}

fn execute_command_palette_index(state: &mut AppState, index: usize) -> Task<Message> {
    let results = crate::commands::command_palette_results(state);
    let visible_len = results
        .len()
        .min(crate::commands::COMMAND_PALETTE_VISIBLE_LIMIT);
    if visible_len == 0 {
        return Task::none();
    }

    let index = index.min(visible_len - 1);
    let Some(command) = results.into_iter().take(visible_len).nth(index) else {
        return Task::none();
    };
    close_command_palette(state);
    update(state, command.message)
}

pub fn handle(state: &mut AppState, message: Message) -> Task<Message> {
    match message {
        Message::TogglePreviewPanel => {
            state.preview_panel_visible = !state.preview_panel_visible;
            state.responsive_right_panel_dismissed = false;
            clear_preview_state(state);
            if state.preview_panel_visible || state.properties_panel_visible {
                return refresh_preview_for_selection(state);
            }
        }
        Message::ToggleQuickLook => {
            if state.quick_look_open {
                return close_quick_look(state);
            }
            state.quick_look_open = true;
            return refresh_preview_for_selection(state);
        }
        Message::CloseQuickLook => {
            return close_quick_look(state);
        }
        Message::QuickLookSelectNext => {
            return select_next_quick_look_item(state);
        }
        Message::QuickLookSelectPrevious => {
            return select_previous_quick_look_item(state);
        }
        Message::OpenCommandPalette => {
            return open_command_palette(state);
        }
        Message::CloseCommandPalette => {
            close_command_palette(state);
        }
        Message::CommandPaletteInputChanged(value) => {
            state.command_palette_query = value;
            state.command_palette_selected = 0;
        }
        Message::CommandPaletteMoveNext => {
            move_command_palette_selection(state, 1);
        }
        Message::CommandPaletteMovePrevious => {
            move_command_palette_selection(state, -1);
        }
        Message::ExecuteCommandPaletteSelection => {
            return execute_command_palette_index(state, state.command_palette_selected);
        }
        Message::ExecuteCommandPaletteIndex(index) => {
            return execute_command_palette_index(state, index);
        }
        Message::TogglePropertiesPanel => {
            state.properties_panel_visible = !state.properties_panel_visible;
            state.responsive_right_panel_dismissed = false;
            clear_preview_state(state);
            if state.preview_panel_visible || state.properties_panel_visible {
                return refresh_preview_for_selection(state);
            }
        }
        Message::DismissResponsiveSidebar => {
            state.responsive_sidebar_dismissed = true;
        }
        Message::DismissResponsiveRightPanel => {
            state.responsive_right_panel_dismissed = true;
        }
        Message::PreviewDebounced(version) => {
            return preview::start_preview_load(state, version);
        }
        Message::PreviewLoaded(path, data) => {
            if state.preview_target_path.as_ref() == Some(&path) {
                state.preview_loading = false;
                state.preview_failed = data.is_none();
                state.preview_data = data;
            }
        }
        Message::DismissNotification(id) => {
            state.remove_notification(id);
        }
        Message::PruneExpiredNotifications => {
            state.remove_expired_notifications();
        }
        Message::SkipError(_job_id) => {
            if let Some(pending) = &mut state.pending_operation {
                if pending.current_index < pending.items.len() {
                    app_runtime().skip_task(&pending.job_id, pending.current_index);
                    pending.current_index += 1;
                }
                if let Some(job) = state.jobs.iter_mut().find(|job| job.id == pending.job_id) {
                    job.state = JobState::Running;
                }
            }
            return process_pending_operation(state);
        }
        Message::AbortJob(job_id) => {
            app_runtime().cancel_job(&job_id);
            if let Some(pending) = &state.pending_operation {
                if pending.job_id == job_id {
                    state.pending_operation = None;
                    state.active_dialog = None;
                }
            }
        }
        Message::SetPreviewPosition(position) => {
            state.preview_position = position;
            if let Some(DialogState::Settings {
                preview_position: current,
                ..
            }) = &mut state.active_dialog
            {
                *current = position;
            }
            return Task::perform(async {}, |_| Message::SaveLayoutConfig);
        }
        Message::SetPropertiesPosition(position) => {
            state.properties_position = position;
            if let Some(DialogState::Settings {
                properties_position,
                ..
            }) = &mut state.active_dialog
            {
                *properties_position = position;
            }
            return Task::perform(async {}, |_| Message::SaveLayoutConfig);
        }
        Message::FolderStatsLoaded(path, stats) => {
            if state.focused_folder_stats_path.as_deref() == Some(&path) {
                state.focused_folder_stats = Some(stats);
            }
        }
        Message::ToggleDualPane => {
            state.dual_pane_enabled = !state.dual_pane_enabled;
            if state.dual_pane_enabled && state.pane(1).current_path.is_empty() {
                let path = state.pane(0).current_path.clone();
                state.pane_mut(1).current_path = path.clone();
                state.pane_mut(1).push_to_history(path.clone());
                refresh_breadcrumbs_for_pane(state, 1);
                return load_secondary_directory_task(path);
            }
            if !state.dual_pane_enabled {
                state.active_pane = 0;
            }
            return Task::perform(async {}, |_| Message::SaveLayoutConfig);
        }
        Message::SetActivePane(pane) => {
            state.active_pane = pane;
        }
        Message::CopyToOtherPane | Message::MoveToOtherPane => {
            let is_move = matches!(message, Message::MoveToOtherPane);
            let other_pane = if state.active_pane == 0 { 1 } else { 0 };
            let source_pane = state.active_pane;
            let (source_entries, source_indices) = (
                &state.pane(source_pane).entries,
                &state.pane(source_pane).selected_indices,
            );
            let destination_path = &state.pane(other_pane).current_path;

            if destination_path.is_empty() || source_indices.is_empty() {
                return Task::none();
            }

            let sources: Vec<ObjectRef> = source_indices
                .iter()
                .filter_map(|&index| {
                    source_entries
                        .get(index as usize)
                        .map(|entry| entry.object_ref.clone())
                })
                .collect();

            if sources.is_empty() {
                return Task::none();
            }

            let destination_dir = PathBuf::from(destination_path);
            let items: Vec<PendingFileItem> = sources
                .iter()
                .map(|source| PendingFileItem {
                    source: source.clone(),
                    destination_path: destination_dir
                        .join(file_name_from_path(&source.display_path))
                        .to_string_lossy()
                        .to_string(),
                })
                .collect();
            return start_pending_file_operation(state, items, is_move);
        }
        Message::SaveLayoutConfig => {
            state.config_save_generation = state.config_save_generation.wrapping_add(1);
            let generation = state.config_save_generation;
            return Task::perform(
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    generation
                },
                Message::FlushLayoutConfig,
            );
        }
        Message::FlushLayoutConfig(generation) => {
            if generation != u64::MAX && generation != state.config_save_generation {
                return Task::none();
            }
            state.config_save_generation = state.config_save_generation.wrapping_add(1);
            return save_layout_config_now(state);
        }
        Message::LayoutConfigLoaded(config) => {
            let window_task = restore_window_task(&config);
            state.sidebar_width = config.sidebar_width;
            state.right_panel_width = config.right_panel_width;
            state.preview_panel_height = config.preview_panel_height;
            state.window_width = config.window_width;
            state.window_height = config.window_height;
            state.window_x = config.window_x;
            state.window_y = config.window_y;
            state.preview_position = config.preview_position;
            state.dual_pane_enabled = config.dual_pane_enabled;
            state.pane_split_ratio = config.pane_split_ratio;
            state.sidebar_visible = config.sidebar_visible;
            state.preview_panel_visible = config.preview_panel_visible;
            state.properties_panel_visible = config.properties_panel_visible;
            state.show_hidden_files = config.show_hidden_files;
            state.cache_encryption_policy = config.cache_encryption_policy;
            state.thumbnail_cache_limit_bytes = config.thumbnail_cache_limit_bytes;
            state.memory_budget_limit_bytes = config.memory_budget_limit_bytes;
            iris_core_domain::memory_budget::MemoryBudget::global()
                .set_cap_bytes(config.memory_budget_limit_bytes);
            state.map_tile_fetch_enabled = config.map_tile_fetch_enabled;
            state.theme_choice = config.theme;
            state.locale = config.locale;
            state.shortcut_overrides = config.shortcut_overrides;
            state.remote_copy_undo_enabled = config.remote_copy_undo_enabled;
            state.notification_timeout_secs = config.notification_timeout_secs;
            state.refresh_notification_expirations();
            crate::i18n::initialize(state.locale.as_deref());
            trim_thumbnail_cache_to_limits(state);
            app_runtime().set_cache_encryption_policy(config.cache_encryption_policy);
            let restored_tabs = state.restore_pane_tabs(&config.tabs);
            {
                let primary = state.pane_mut(0);
                primary.list_columns = config.list_columns.clone();
            }
            {
                let secondary = state.pane_mut(1);
                secondary.list_columns = config.secondary_list_columns.clone();
            }
            state.properties_split_ratio = config.properties_split_ratio;
            state.properties_position = config.properties_position;
            if !config.sidebar_groups.is_empty() {
                restore_sidebar_groups(state, &config.sidebar_groups);
            }
            state.remote_transport_mappings = config.remote_transport_mappings;
            app_runtime().set_remote_transport_mappings(state.remote_transport_mappings.clone());
            for warning in config.remote_transport_secret_warnings {
                let mut args = FluentArgs::new();
                args.set("host", warning.host);
                args.set("error", warning.error);
                state.add_notification(
                    NotificationSeverity::Warning,
                    tr("notification-remote-secret-load-failed-title"),
                    crate::i18n::tr_args(
                        "notification-remote-secret-load-failed-body",
                        Some(&args),
                    ),
                    vec![],
                );
            }
            clear_preview_state(state);
            let preview_task = if state.preview_panel_visible || state.properties_panel_visible {
                refresh_preview_for_selection(state)
            } else {
                Task::none()
            };
            if restored_tabs {
                refresh_breadcrumbs_for_pane(state, 0);
                refresh_breadcrumbs_for_pane(state, 1);
                let primary_path = state.pane(0).current_path.clone();
                let secondary_path = state.pane(1).current_path.clone();
                let primary_task = if primary_path.is_empty() {
                    Task::none()
                } else {
                    state.pane_mut(0).load_state = PaneLoadState::LoadingInitial;
                    load_directory_progressively(primary_path, state.show_hidden_files)
                };
                let secondary_task = if secondary_path.is_empty() {
                    Task::none()
                } else {
                    state.pane_mut(1).load_state = PaneLoadState::LoadingInitial;
                    load_secondary_directory_task(secondary_path)
                };
                return Task::batch([preview_task, window_task, primary_task, secondary_task]);
            }
            return Task::batch([preview_task, window_task]);
        }
        Message::LayoutConfigSaved(Err(error)) => {
            tracing::warn!("failed to save layout config: {}", error);
        }
        _ => {}
    }

    Task::none()
}

/// System groups keep their live items and only take the saved collapsed state; the other groups
/// are replaced by the saved ones.
fn restore_sidebar_groups(state: &mut AppState, saved: &[crate::config::SidebarGroupConfig]) {
    state.sidebar_groups.retain(SidebarGroup::is_system);
    for group in saved {
        let Some(kind) = group.kind.or_else(|| legacy_sidebar_group_kind(group)) else {
            continue;
        };
        if kind.is_system() {
            if let Some(existing) = state
                .sidebar_groups
                .iter_mut()
                .find(|current| current.kind == kind)
            {
                existing.collapsed = group.collapsed;
            }
            continue;
        }
        state.sidebar_groups.push(SidebarGroup {
            name: group.name.clone(),
            collapsed: group.collapsed,
            items: group
                .items
                .iter()
                .map(|item| SidebarItem {
                    name: item.name.clone(),
                    path: item.path.clone(),
                    badge: item.badge.clone(),
                    badge_color: item.badge_color,
                    ejectable: item.ejectable,
                })
                .collect(),
            kind,
        });
    }
}

/// Files written before group kinds were saved only had translated names; match them against
/// every bundled language. Unknown system groups are dropped (they are rebuilt at start-up).
fn legacy_sidebar_group_kind(
    group: &crate::config::SidebarGroupConfig,
) -> Option<crate::state::SidebarGroupKind> {
    use crate::state::SidebarGroupKind;

    let named = |kind: SidebarGroupKind| {
        crate::i18n::translations_of(kind.default_name_key()).contains(&group.name)
    };
    if group.system {
        [SidebarGroupKind::Places, SidebarGroupKind::Volumes]
            .into_iter()
            .find(|kind| named(*kind))
    } else if named(SidebarGroupKind::Favorites) {
        Some(SidebarGroupKind::Favorites)
    } else if named(SidebarGroupKind::Cloud) {
        Some(SidebarGroupKind::Cloud)
    } else {
        Some(SidebarGroupKind::Custom)
    }
}

#[cfg(test)]
mod sidebar_restore_tests {
    use super::*;
    use crate::config::SidebarGroupConfig;
    use crate::state::SidebarGroupKind;

    fn saved(name: &str, system: bool, kind: Option<SidebarGroupKind>) -> SidebarGroupConfig {
        SidebarGroupConfig {
            name: name.to_string(),
            collapsed: true,
            items: Vec::new(),
            system,
            kind,
        }
    }

    #[test]
    fn restore_matches_system_groups_by_kind_whatever_the_saved_language() {
        crate::i18n::initialize(Some("ja"));
        let places_ja = tr("sidebar-group-places");
        let favorites_ja = tr("sidebar-group-favorites");
        crate::i18n::initialize(Some("en"));
        let mut state = AppState::default();
        state.sidebar_groups = vec![
            SidebarGroup::new(SidebarGroupKind::Places),
            SidebarGroup::new(SidebarGroupKind::Volumes),
        ];

        restore_sidebar_groups(
            &mut state,
            &[
                // Written by an older version in Japanese: kinds are inferred from the names.
                saved(&places_ja, true, None),
                saved(&favorites_ja, false, None),
                saved("Work", false, None),
                saved("Volumes", true, Some(SidebarGroupKind::Volumes)),
            ],
        );

        let kinds = state
            .sidebar_groups
            .iter()
            .map(|group| (group.kind, group.collapsed))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                (SidebarGroupKind::Places, true),
                (SidebarGroupKind::Volumes, true),
                (SidebarGroupKind::Favorites, true),
                (SidebarGroupKind::Custom, true),
            ]
        );
        crate::i18n::initialize(None);
    }
}
