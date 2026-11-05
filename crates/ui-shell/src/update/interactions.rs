use super::*;

fn external_dnd_target(state: &AppState) -> Option<DndTarget> {
    let pane = state
        .mouse_position
        .map(|position| pane_under_cursor(state, position))
        .unwrap_or(state.active_pane);

    let current_path = state.pane(pane).current_path.trim();
    if current_path.is_empty() {
        None
    } else {
        Some(DndTarget::PaneDirectory { pane })
    }
}

fn list_column_resized_width(kind: ListColumnKind, value: f32) -> f32 {
    match kind {
        ListColumnKind::Name => value.max(0.0),
        ListColumnKind::Size => value.clamp(50.0, 300.0),
        ListColumnKind::Date => value.clamp(60.0, 300.0),
    }
}

pub fn handle(state: &mut AppState, message: Message) -> Task<Message> {
    match message {
        Message::DragStart { pane, index } => {
            let selected_indices = &state.pane(pane).selected_indices;
            let source_indices = if selected_indices.contains(&index) {
                selected_indices.iter().copied().collect::<Vec<_>>()
            } else {
                vec![index]
            };
            let origin = state.mouse_position.unwrap_or(iced::Point::ORIGIN);
            state.dnd_state = DndState::Pressed {
                source_pane: pane,
                source_indices,
                origin,
                target: None,
            };
        }
        Message::DragHover(target) => match (&mut state.dnd_state, target) {
            (
                DndState::Pressed {
                    target: current, ..
                },
                target,
            )
            | (
                DndState::Dragging {
                    target: current, ..
                },
                target,
            )
            | (
                DndState::ExternalDragging {
                    target: current, ..
                },
                target,
            ) => {
                *current = Some(target);
            }
            (DndState::Idle, _) => {}
        },
        Message::DragLeave(target) => match (&mut state.dnd_state, target) {
            (
                DndState::Pressed {
                    target: current, ..
                },
                DndTarget::Entry { pane, index },
            )
            | (
                DndState::Dragging {
                    target: current, ..
                },
                DndTarget::Entry { pane, index },
            )
            | (
                DndState::ExternalDragging {
                    target: current, ..
                },
                DndTarget::Entry { pane, index },
            ) => {
                if *current == Some(DndTarget::Entry { pane, index }) {
                    *current = Some(DndTarget::PaneDirectory { pane });
                }
            }
            (
                DndState::Pressed {
                    target: current, ..
                },
                DndTarget::PaneDirectory { pane },
            )
            | (
                DndState::Dragging {
                    target: current, ..
                },
                DndTarget::PaneDirectory { pane },
            )
            | (
                DndState::ExternalDragging {
                    target: current, ..
                },
                DndTarget::PaneDirectory { pane },
            ) => {
                if *current == Some(DndTarget::PaneDirectory { pane }) {
                    *current = None;
                }
            }
            (DndState::Idle, _) => {}
        },
        Message::DragEnd => {
            state.dnd_state = DndState::Idle;
            state.pending_external_drop_paths.clear();
            state.pending_external_drop_target = None;
        }
        Message::WindowEvent(_window_id, event) => match event {
            window::Event::Opened { position, size } => {
                state.window_width = size.width;
                state.window_height = size.height;
                if let Some(position) = position {
                    state.window_x = Some(position.x);
                    state.window_y = Some(position.y);
                }
            }
            window::Event::Moved(position) => {
                state.window_x = Some(position.x);
                state.window_y = Some(position.y);
                return Task::done(Message::SaveLayoutConfig);
            }
            window::Event::Resized(size) => {
                state.window_width = size.width;
                state.window_height = size.height;
                return Task::done(Message::SaveLayoutConfig);
            }
            window::Event::FileHovered(path) => {
                let target = external_dnd_target(state);
                match &mut state.dnd_state {
                    DndState::ExternalDragging {
                        source_paths,
                        target: current_target,
                    } => {
                        if !source_paths.iter().any(|existing| existing == &path) {
                            source_paths.push(path);
                        }
                        *current_target = target;
                    }
                    DndState::Idle => {
                        state.dnd_state = DndState::ExternalDragging {
                            source_paths: vec![path],
                            target,
                        };
                    }
                    DndState::Pressed { .. } | DndState::Dragging { .. } => {}
                }
            }
            window::Event::FileDropped(path) => {
                if !state
                    .pending_external_drop_paths
                    .iter()
                    .any(|existing| existing == &path)
                {
                    state.pending_external_drop_paths.push(path);
                }

                state.pending_external_drop_target = state
                    .dnd_state
                    .target()
                    .or_else(|| external_dnd_target(state));
                state.pending_external_drop_token =
                    state.pending_external_drop_token.wrapping_add(1);
                let token = state.pending_external_drop_token;

                return Task::perform(
                    async move {
                        std::thread::sleep(Duration::from_millis(30));
                        token
                    },
                    Message::CommitExternalDrop,
                );
            }
            window::Event::FilesHoveredLeft
                if matches!(state.dnd_state, DndState::ExternalDragging { .. })
                    && state.pending_external_drop_paths.is_empty() =>
            {
                state.dnd_state = DndState::Idle;
            }
            _ => {}
        },
        Message::CommitExternalDrop(token) => {
            if token != state.pending_external_drop_token {
                return Task::none();
            }

            let source_paths = std::mem::take(&mut state.pending_external_drop_paths);
            let target = state
                .pending_external_drop_target
                .take()
                .or_else(|| state.dnd_state.target())
                .or_else(|| external_dnd_target(state));
            state.dnd_state = DndState::Idle;

            return perform_external_drop(state, source_paths, target);
        }
        Message::KeyPressed(key, modifiers) => {
            state.keyboard_modifiers = modifiers;
            if state.shortcut_recording_action.is_some() {
                if matches!(
                    key,
                    iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
                ) {
                    return update(state, SettingsMessage::CancelShortcutRecording.into());
                }
                if let Some(combo) =
                    crate::input::keyboard::combo_string_from_event(&key, modifiers)
                {
                    return update(state, SettingsMessage::RecordedShortcut(combo).into());
                }
                return Task::none();
            }
            if let Some(message) = crate::input::keyboard::handle_key_event(key, modifiers, state) {
                return update(state, message);
            }
        }
        Message::ModifiersChanged(modifiers) => {
            state.keyboard_modifiers = modifiers;
        }
        Message::Refresh => {
            let pane_index = state.active_pane;
            let path = state.pane(pane_index).current_path.clone();
            let load_state = if state.pane(pane_index).entries.is_empty() {
                PaneLoadState::LoadingInitial
            } else {
                PaneLoadState::LoadingMore
            };
            state.pane_mut(pane_index).load_state = load_state;
            if pane_index == 0 {
                return Task::perform(
                    read_directory(path.clone(), state.show_hidden_files),
                    move |result| match result {
                        Ok(entries) => DirectoryMessage::Polled { path, entries }.into(),
                        Err(error) => DirectoryMessage::LoadFailed { path, error }.into(),
                    },
                );
            }

            let show_hidden = state.show_hidden_files;
            return Task::perform(read_directory(path.clone(), show_hidden), move |result| {
                match result {
                    Ok(entries) => Message::Pane(1, PaneMessage::DirectoryLoaded { path, entries }),
                    Err(error) => {
                        Message::Pane(1, PaneMessage::DirectoryLoadFailed { path, error })
                    }
                }
            });
        }
        Message::SystemThemeChanged(mode) => {
            if state.theme_choice == crate::config::ThemeChoice::System {
                state.system_theme = crate::state::theme_from_mode(mode);
            }
        }
        Message::FocusFilter => {
            let input_id = crate::app::ids::filter_input_id(state.active_pane);
            return Task::batch([
                iced::widget::operation::focus(input_id.clone()),
                iced::widget::operation::select_all(input_id),
            ]);
        }
        Message::ClearFilter => {
            let pane_idx = state.active_pane;
            state.pane_mut(pane_idx).set_filter_query(String::new());
            return refresh_preview_for_selection(state);
        }
        Message::ToggleHiddenFiles => {
            state.show_hidden_files = !state.show_hidden_files;
            return update(state, Message::Refresh);
        }
        Message::WindowResized(size) => {
            state.window_width = size.width;
            state.window_height = size.height;
            let layout = crate::app::resolved_layout::ResolvedLayout::resolve(state);
            if layout.sidebar_inline {
                state.responsive_sidebar_dismissed = false;
            }
            if layout.right_panel_inline {
                state.responsive_right_panel_dismissed = false;
            }
            return Task::done(Message::SaveLayoutConfig);
        }
        Message::BeginWindowDrag => {
            return window::latest().then(|window_id| match window_id {
                Some(window_id) => window::drag(window_id),
                None => Task::none(),
            });
        }
        Message::AppMenuCommand(command) => {
            return match command {
                AppMenuCommand::OpenSelected => update(state, Message::OpenSelected),
                AppMenuCommand::CreateFolder => update(state, Message::CreateFolder),
                AppMenuCommand::EditPath => update(
                    state,
                    NavigationMessage::EditPath {
                        pane: state.active_pane,
                    }
                    .into(),
                ),
                AppMenuCommand::NavigateBack => update(state, NavigationMessage::Back.into()),
                AppMenuCommand::NavigateForward => update(state, NavigationMessage::Forward.into()),
                AppMenuCommand::NavigateUp => update(state, NavigationMessage::Up.into()),
                AppMenuCommand::CutSelected => update(state, Message::CutSelected),
                AppMenuCommand::CopySelected => update(state, Message::CopySelected),
                AppMenuCommand::Paste => update(state, Message::Paste),
                AppMenuCommand::Undo => update(state, Message::Undo),
                AppMenuCommand::Redo => update(state, Message::Redo),
                AppMenuCommand::DeleteSelected => update(state, Message::DeleteSelected),
                AppMenuCommand::SelectAll => update(
                    state,
                    Message::Pane(state.active_pane, PaneMessage::SelectAll),
                ),
                AppMenuCommand::OpenSettings => update(state, SettingsMessage::Open.into()),
                AppMenuCommand::Refresh => update(state, Message::Refresh),
                AppMenuCommand::ToggleSidebar => {
                    update(state, SidebarMessage::ToggleVisibility.into())
                }
                AppMenuCommand::TogglePreviewPanel => update(state, Message::TogglePreviewPanel),
                AppMenuCommand::TogglePropertiesPanel => {
                    update(state, Message::TogglePropertiesPanel)
                }
                AppMenuCommand::ToggleHiddenFiles => update(state, Message::ToggleHiddenFiles),
                AppMenuCommand::OpenHelp => Task::perform(
                    async { super::open_help_document() },
                    Message::OpenHelpCompleted,
                ),
            };
        }
        Message::OpenHelpCompleted(Err(error)) => {
            state.add_notification(
                NotificationSeverity::Error,
                tr("notification-open-help-failed-title"),
                error,
                vec![],
            );
        }
        Message::ConfigFileChanged => {
            if let Some(last_save) = state.last_config_save {
                if last_save.elapsed() >= Duration::from_secs(3) {
                    state.last_config_save = None;
                    return Task::perform(
                        crate::config::load_config(),
                        Message::LayoutConfigLoaded,
                    );
                }
            } else {
                return Task::perform(crate::config::load_config(), Message::LayoutConfigLoaded);
            }
        }
        Message::ResizeStart(target) => {
            let pos = state.mouse_position.unwrap_or(iced::Point::ORIGIN);
            let (start_value, start_value2) = match target {
                ResizeTarget::Sidebar => (state.sidebar_width, 0.0),
                ResizeTarget::RightPanel => (state.right_panel_width, 0.0),
                ResizeTarget::PreviewPanel => (state.preview_panel_height, 0.0),
                ResizeTarget::PaneDivider => {
                    let layout = crate::app::resolved_layout::ResolvedLayout::resolve(state);
                    let handle_w = crate::views::theme::RESIZE_HANDLE_WIDTH;
                    (layout.main_left, (layout.main_width - handle_w).max(1.0))
                }
                ResizeTarget::PropertiesDivider => {
                    let layout = crate::app::resolved_layout::ResolvedLayout::resolve(state);
                    let handle_w = crate::views::theme::RESIZE_HANDLE_WIDTH;
                    let properties_position = state.properties_position;
                    if properties_position.is_horizontal() {
                        let container_w = match state.preview_position {
                            PreviewPosition::Top | PreviewPosition::Bottom
                                if state.preview_panel_visible =>
                            {
                                layout.main_width
                            }
                            _ => layout.right_panel_width,
                        };
                        let available = (container_w - handle_w).max(1.0);
                        (state.properties_split_ratio, available)
                    } else {
                        let container_h = match state.preview_position {
                            PreviewPosition::Top | PreviewPosition::Bottom
                                if state.preview_panel_visible =>
                            {
                                state.preview_panel_height
                            }
                            _ => state.window_height,
                        };
                        let available = (container_h - handle_w).max(1.0);
                        (state.properties_split_ratio, available)
                    }
                }
                ResizeTarget::ListColumnBoundary { pane, left, right } => (
                    state.pane(pane).list_column_width(left),
                    state.pane(pane).list_column_width(right),
                ),
            };
            state.resize_state = Some(ResizeState {
                target,
                start_mouse_x: pos.x,
                start_mouse_y: pos.y,
                start_value,
                start_value2,
            });
        }
        Message::MouseMoved(position) => {
            state.mouse_position = Some(position);

            if let Some(resize) = state.resize_state.clone() {
                let dx = position.x - resize.start_mouse_x;
                let dy = position.y - resize.start_mouse_y;
                match resize.target {
                    ResizeTarget::Sidebar => {
                        let minimum_main = if state.dual_pane_enabled {
                            crate::app::resolved_layout::MIN_DUAL_PANE_WIDTH * 2.0
                                + crate::views::theme::RESIZE_HANDLE_WIDTH
                        } else {
                            crate::app::resolved_layout::MIN_SINGLE_PANE_WIDTH
                        };
                        let maximum = (state.window_width
                            - minimum_main
                            - crate::views::theme::RESIZE_HANDLE_WIDTH)
                            .clamp(
                                crate::views::theme::SIDEBAR_MIN_WIDTH,
                                crate::views::theme::SIDEBAR_MAX_WIDTH,
                            );
                        state.sidebar_width = (resize.start_value + dx)
                            .clamp(crate::views::theme::SIDEBAR_MIN_WIDTH, maximum);
                    }
                    ResizeTarget::RightPanel => {
                        let delta = if state.preview_position == PreviewPosition::Left {
                            dx
                        } else {
                            -dx
                        };
                        let layout = crate::app::resolved_layout::ResolvedLayout::resolve(state);
                        let minimum_main = if state.dual_pane_enabled {
                            crate::app::resolved_layout::MIN_DUAL_PANE_WIDTH * 2.0
                                + crate::views::theme::RESIZE_HANDLE_WIDTH
                        } else {
                            crate::app::resolved_layout::MIN_SINGLE_PANE_WIDTH
                        };
                        let sidebar_span = if layout.sidebar_inline {
                            layout.sidebar_width + crate::views::theme::RESIZE_HANDLE_WIDTH
                        } else {
                            0.0
                        };
                        let maximum = (state.window_width
                            - sidebar_span
                            - minimum_main
                            - crate::views::theme::RESIZE_HANDLE_WIDTH)
                            .clamp(
                                crate::views::theme::RIGHT_PANEL_MIN_WIDTH,
                                crate::views::theme::RIGHT_PANEL_MAX_WIDTH,
                            );
                        state.right_panel_width = (resize.start_value + delta)
                            .clamp(crate::views::theme::RIGHT_PANEL_MIN_WIDTH, maximum);
                    }
                    ResizeTarget::PreviewPanel => {
                        let new_height = if state.preview_position == PreviewPosition::Top {
                            resize.start_value + dy
                        } else {
                            resize.start_value - dy
                        };
                        state.preview_panel_height = new_height.clamp(
                            crate::views::theme::PREVIEW_PANEL_MIN_HEIGHT,
                            (state.window_height - 240.0)
                                .max(crate::views::theme::PREVIEW_PANEL_MIN_HEIGHT),
                        );
                    }
                    ResizeTarget::PaneDivider => {
                        let available = resize.start_value2;
                        if available > 0.0 {
                            let rel = (position.x - resize.start_value) / available;
                            let minimum_ratio = (crate::app::resolved_layout::MIN_DUAL_PANE_WIDTH
                                / available)
                                .min(0.5);
                            state.pane_split_ratio = rel.clamp(minimum_ratio, 1.0 - minimum_ratio);
                        }
                    }
                    ResizeTarget::PropertiesDivider => {
                        let available = resize.start_value2;
                        if available > 0.0 {
                            let properties_position = state.properties_position;
                            let delta = if properties_position.is_horizontal() {
                                dx
                            } else {
                                dy
                            };
                            let next_ratio = if properties_position.properties_first() {
                                resize.start_value - (delta / available)
                            } else {
                                resize.start_value + (delta / available)
                            };
                            state.properties_split_ratio = next_ratio.clamp(0.15, 0.85);
                        }
                    }
                    ResizeTarget::ListColumnBoundary { pane, left, right } => {
                        let pane = state.pane_mut(pane);
                        if left.is_flexible() && !right.is_flexible() {
                            pane.set_list_column_width(
                                right,
                                list_column_resized_width(right, resize.start_value2 - dx),
                            );
                        } else if right.is_flexible() && !left.is_flexible() {
                            pane.set_list_column_width(
                                left,
                                list_column_resized_width(left, resize.start_value + dx),
                            );
                        } else {
                            pane.set_list_column_width(
                                left,
                                list_column_resized_width(left, resize.start_value + dx),
                            );
                            pane.set_list_column_width(
                                right,
                                list_column_resized_width(right, resize.start_value2 - dx),
                            );
                        }
                    }
                }
            }

            if let DndState::Pressed {
                source_pane,
                source_indices,
                origin,
                target,
            } = &state.dnd_state
            {
                let dx = position.x - origin.x;
                let dy = position.y - origin.y;
                if (dx * dx + dy * dy).sqrt() >= crate::input::dnd::DRAG_THRESHOLD_PX {
                    state.dnd_state = DndState::Dragging {
                        source_pane: *source_pane,
                        source_indices: source_indices.clone(),
                        target: *target,
                    };
                }
            } else if matches!(state.dnd_state, DndState::ExternalDragging { .. }) {
                state.dnd_state.set_target(external_dnd_target(state));
            }
        }
        Message::ResizeEnd => {
            let mut tasks = Vec::new();
            state.context_menu = None;

            if state.resize_state.is_some() {
                state.resize_state = None;
                tasks.push(Task::done(Message::FlushLayoutConfig(u64::MAX)));
            }

            if state.tab_drag_state.is_some() {
                tasks.push(tabs::finish_tab_drag(state));
            }

            match state.dnd_state {
                DndState::Dragging { .. } => {
                    tasks.push(perform_dnd_drop(state));
                }
                DndState::Pressed { .. } => {
                    state.dnd_state = DndState::Idle;
                }
                DndState::ExternalDragging { .. } | DndState::Idle => {}
            }

            if !tasks.is_empty() {
                return Task::batch(tasks);
            }
        }
        _ => {}
    }

    Task::none()
}
