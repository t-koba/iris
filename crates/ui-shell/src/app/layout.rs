use super::pane_shell::view_pane_content;
use super::toolbar::{view_status_bar, view_title_bar_controls, view_toolbar};
use crate::config::ThemeChoice;
use crate::format::truncate_with_ellipsis;
use crate::i18n::{tr, tr_with};
use crate::input::dnd::{DndOperation, DndState};
use crate::messages::{DialogMessage, Message, SidebarMessage, TabMessage};
use crate::runtime::app_runtime;
use crate::state::{
    AppState, ContextMenuKind, ContextMenuState, DialogState, PreviewPosition, ResizeState,
    ResizeTarget,
};
use crate::update::file_ops::current_dnd_operation;
use crate::views::{conflict_dialog, dialogs, notifications, preview, theme};
use crate::widgets::context_menu_overlay::ContextMenuOverlay;
use iced::widget::{
    button, canvas, column, container, mouse_area, row, rule, scrollable, space, stack, text,
    text_input, Column, Row,
};
use iced::{mouse, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

pub fn title(state: &AppState) -> String {
    if state.pane(0).current_path.is_empty() {
        "Iris".to_string()
    } else {
        format!(
            "Iris — {}",
            app_runtime().format_display_path(&state.pane(0).current_path)
        )
    }
}

pub fn theme(state: &AppState) -> Theme {
    match state.theme_choice {
        ThemeChoice::System => {
            if state.system_theme.extended_palette().is_dark {
                theme::iris_dark()
            } else {
                theme::iris_light()
            }
        }
        ThemeChoice::Light => theme::iris_light(),
        ThemeChoice::Dark => theme::iris_dark(),
    }
}

pub fn view(state: &AppState) -> Element<'_, Message> {
    #[cfg(target_os = "macos")]
    crate::macos_menu::install_if_needed();

    view_content(state)
}

/// Builds the whole UI without platform hooks, keeping layout tests deterministic and thread-safe.
#[doc(hidden)]
pub fn view_for_test(state: &AppState) -> Element<'_, Message> {
    view_content(state)
}

fn view_content(state: &AppState) -> Element<'_, Message> {
    let main = view_main_layout(state);
    let main = if state.quick_look_open {
        view_with_quick_look(state, main)
    } else {
        main
    };
    let main = if state.command_palette_open {
        view_with_command_palette(state, main)
    } else {
        main
    };
    let main = if drag_ghost_data(state).is_some() && state.active_dialog.is_none() {
        view_with_drag_ghost(state, main)
    } else {
        main
    };
    let main = if state.context_menu.is_some() && state.active_dialog.is_none() {
        view_with_context_menu(state, main)
    } else {
        main
    };

    if let Some(dialog) = &state.active_dialog {
        return view_with_dialog(state, dialog, main);
    }

    main
}

fn view_with_dialog<'a>(
    state: &'a AppState,
    dialog: &'a DialogState,
    main: Element<'a, Message>,
) -> Element<'a, Message> {
    let dialog_content = match dialog {
        DialogState::DeleteConfirmation {
            target_names,
            use_trash,
            can_use_trash,
            ..
        } => dialogs::view_delete_dialog(target_names, *use_trash, *can_use_trash),
        DialogState::CreateFolder { input_value } => {
            dialogs::view_create_folder_dialog(input_value)
        }
        DialogState::Error { title, message } => dialogs::view_error_dialog(title, message),
        DialogState::Conflict {
            source,
            destination_path,
            source_size,
            destination_size,
            source_mtime,
            destination_mtime,
            remember_choice,
        } => conflict_dialog::view_conflict_dialog(
            &source.display_path,
            destination_path,
            *source_size,
            *destination_size,
            *source_mtime,
            *destination_mtime,
            *remember_choice,
        ),
        DialogState::Settings {
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
        } => dialogs::view_settings_dialog(dialogs::SettingsDialogView {
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
            shortcut_recording_action: state.shortcut_recording_action.as_deref(),
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
        }),
        DialogState::ConnectNetwork { form } => dialogs::view_connect_network_dialog(form),
        DialogState::ConnectCloud { form } => dialogs::view_connect_cloud_dialog(form),
        DialogState::BatchRename { state } => dialogs::view_batch_rename_dialog(state),
        DialogState::RemoteUndoConfirmation { item_count, .. } => {
            dialogs::view_remote_undo_confirmation_dialog(*item_count)
        }
    };

    // A transfer waits on the conflict question, so a stray click beside it must not answer it.
    let closes_on_backdrop = !matches!(dialog, DialogState::Conflict { .. });
    let backdrop = mouse_area(
        container(
            iced::widget::Space::new()
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .style(crate::views::theme::dialog_backdrop_style)
        .width(Length::Fill)
        .height(Length::Fill),
    );
    let backdrop = if closes_on_backdrop {
        backdrop.on_press(DialogMessage::Close.into())
    } else {
        backdrop
    };

    iced::widget::stack![
        main,
        backdrop,
        container(dialog_content).center(Length::Fill)
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

const DND_GHOST_WIDTH: f32 = 190.0;
const DND_GHOST_HEIGHT: f32 = 52.0;

#[derive(Clone, Debug)]
struct DragGhostData {
    title: String,
    operation: String,
    count_label: String,
}

fn view_with_drag_ghost<'a>(
    state: &'a AppState,
    main: Element<'a, Message>,
) -> Element<'a, Message> {
    let Some(data) = drag_ghost_data(state) else {
        return main;
    };
    let Some(position) = state.mouse_position else {
        return main;
    };
    let x = (position.x + 14.0).clamp(0.0, (state.window_width - DND_GHOST_WIDTH).max(0.0));
    let y = (position.y + 18.0).clamp(0.0, (state.window_height - DND_GHOST_HEIGHT).max(0.0));
    let overlay = column![
        space::vertical().height(Length::Fixed(y)),
        row![
            space::horizontal().width(Length::Fixed(x)),
            canvas::Canvas::new(DragGhostCanvas { data })
                .width(Length::Fixed(DND_GHOST_WIDTH))
                .height(Length::Fixed(DND_GHOST_HEIGHT))
        ]
        .width(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    stack![main, overlay]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn drag_ghost_data(state: &AppState) -> Option<DragGhostData> {
    let (count, title) = match &state.dnd_state {
        DndState::Dragging {
            source_pane,
            source_indices,
            ..
        } => {
            let count = source_indices.len();
            let title = source_indices
                .first()
                .and_then(|index| state.pane(*source_pane).entry(*index))
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| tr("dnd-ghost-title-fallback"));
            (count, title)
        }
        DndState::ExternalDragging { source_paths, .. } => {
            let count = source_paths.len();
            let title = source_paths
                .first()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| tr("dnd-ghost-title-fallback"));
            (count, title)
        }
        DndState::Pressed { .. } | DndState::Idle => return None,
    };
    if count == 0 {
        return None;
    }

    let operation = current_dnd_operation(state)
        .map(dnd_operation_label)
        .unwrap_or_else(|| tr("dnd-operation-move"));
    Some(DragGhostData {
        title: truncate_with_ellipsis(&title, 28),
        operation,
        count_label: tr_with("dnd-ghost-items", &[("count", count.into())]),
    })
}

fn dnd_operation_label(operation: DndOperation) -> String {
    match operation {
        DndOperation::Copy => tr("dnd-operation-copy"),
        DndOperation::Move => tr("dnd-operation-move"),
        DndOperation::Link => tr("dnd-operation-link"),
    }
}

#[derive(Clone, Debug)]
struct DragGhostCanvas {
    data: DragGhostData,
}

impl canvas::Program<Message> for DragGhostCanvas {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        frame.fill_rectangle(
            Point::ORIGIN,
            Size::new(DND_GHOST_WIDTH, DND_GHOST_HEIGHT),
            Color::from_rgba8(26, 30, 36, 0.92),
        );
        frame.stroke_rectangle(
            Point::ORIGIN,
            Size::new(DND_GHOST_WIDTH, DND_GHOST_HEIGHT),
            canvas::Stroke::default()
                .with_color(Color::from_rgba8(160, 176, 196, 0.55))
                .with_width(1.0),
        );
        frame.fill_text(canvas::Text {
            content: self.data.title.clone(),
            position: Point::new(12.0, 19.0),
            color: Color::from_rgb8(245, 247, 250),
            size: iced::Pixels(12.0),
            ..canvas::Text::default()
        });
        frame.fill_text(canvas::Text {
            content: self.data.operation.clone(),
            position: Point::new(12.0, 38.0),
            color: Color::from_rgb8(174, 184, 198),
            size: iced::Pixels(10.0),
            ..canvas::Text::default()
        });

        let badge_width = 58.0;
        let badge_origin = Point::new(DND_GHOST_WIDTH - badge_width - 10.0, 12.0);
        frame.fill_rectangle(
            badge_origin,
            Size::new(badge_width, 24.0),
            Color::from_rgb8(61, 111, 214),
        );
        frame.fill_text(canvas::Text {
            content: self.data.count_label.clone(),
            position: Point::new(badge_origin.x + 8.0, badge_origin.y + 16.0),
            color: Color::WHITE,
            size: iced::Pixels(10.0),
            ..canvas::Text::default()
        });
        vec![frame.into_geometry()]
    }
}

fn view_with_context_menu<'a>(
    state: &'a AppState,
    main: Element<'a, Message>,
) -> Element<'a, Message> {
    let Some(menu) = state.context_menu.as_ref() else {
        return main;
    };
    ContextMenuOverlay::new(
        main,
        view_context_menu(menu),
        menu.position,
        Message::CloseContextMenu,
    )
    .into()
}

fn view_context_menu(menu: &ContextMenuState) -> Element<'_, Message> {
    let target = truncate_with_ellipsis(&menu.target_name, 30);
    let base = column![
        container(text(target).size(11))
            .padding([2, 8])
            .width(Length::Fill),
        rule::horizontal(1),
    ]
    .spacing(2);
    let content = match &menu.kind {
        ContextMenuKind::Item { .. } => base
            .push(context_menu_action(
                "\u{E89E}",
                tr("common-open"),
                Message::ItemActionOpen,
                false,
            ))
            .push(context_menu_action(
                "\u{E14D}",
                tr("common-copy"),
                Message::ItemActionCopy,
                false,
            ))
            .push(context_menu_action(
                "\u{E14E}",
                tr("common-cut"),
                Message::ItemActionCut,
                false,
            ))
            .push(context_menu_action(
                "\u{E9A2}",
                tr("common-rename"),
                Message::ItemActionRename,
                false,
            ))
            .push(rule::horizontal(1))
            .push(context_menu_action(
                "\u{E92E}",
                tr("common-delete"),
                Message::ItemActionDelete,
                true,
            )),
        ContextMenuKind::SidebarItem {
            group,
            item,
            removable,
        } => {
            let content = base
                .push(context_menu_action(
                    "\u{E89E}",
                    tr("common-open"),
                    Message::SidebarContextOpen,
                    false,
                ))
                .push(context_menu_action(
                    "\u{E89E}",
                    tr("sidebar-open-new-window"),
                    Message::SidebarContextOpenInNewWindow,
                    false,
                ));
            if *removable {
                content.push(rule::horizontal(1)).push(context_menu_action(
                    "\u{E92E}",
                    tr("common-delete"),
                    SidebarMessage::RemoveItem {
                        group: *group,
                        item: *item,
                    }
                    .into(),
                    true,
                ))
            } else {
                content
            }
        }
        ContextMenuKind::Tab {
            pane,
            tab,
            can_close,
            can_move_left,
            can_move_right,
        } => base
            .push(context_menu_action(
                "\u{E5D5}",
                tr("tab-switch-action"),
                TabMessage::Switch {
                    pane: *pane,
                    tab: *tab,
                }
                .into(),
                false,
            ))
            .push(context_menu_action(
                "\u{E14D}",
                tr("tab-duplicate"),
                TabMessage::Duplicate {
                    pane: *pane,
                    tab: *tab,
                }
                .into(),
                false,
            ))
            .push(rule::horizontal(1))
            .push(context_menu_action_maybe(
                "\u{E5CB}",
                tr("tab-move-left"),
                can_move_left.then_some(
                    TabMessage::MoveLeft {
                        pane: *pane,
                        tab: *tab,
                    }
                    .into(),
                ),
                false,
            ))
            .push(context_menu_action_maybe(
                "\u{E5CC}",
                tr("tab-move-right"),
                can_move_right.then_some(
                    TabMessage::MoveRight {
                        pane: *pane,
                        tab: *tab,
                    }
                    .into(),
                ),
                false,
            ))
            .push(rule::horizontal(1))
            .push(context_menu_action_maybe(
                "\u{E5CD}",
                tr("tab-close"),
                can_close.then_some(
                    TabMessage::Close {
                        pane: *pane,
                        tab: *tab,
                    }
                    .into(),
                ),
                true,
            )),
    };

    container(content)
        .width(Length::Fixed(220.0))
        .padding(6)
        .style(theme::dialog_card_style)
        .into()
}

fn context_menu_action(
    icon: &str,
    label: String,
    message: Message,
    danger: bool,
) -> Element<'_, Message> {
    context_menu_action_maybe(icon, label, Some(message), danger)
}

fn context_menu_action_maybe(
    icon: &str,
    label: String,
    message: Option<Message>,
    danger: bool,
) -> Element<'_, Message> {
    let content = row![theme::icon(icon).size(14), text(label).size(12)]
        .spacing(8)
        .align_y(iced::Alignment::Center);
    let button = button(content)
        .on_press_maybe(message)
        .padding([5, 8])
        .width(Length::Fill);
    if danger {
        button.style(button::danger).into()
    } else {
        button.style(button::text).into()
    }
}

fn view_with_command_palette<'a>(
    state: &'a AppState,
    main: Element<'a, Message>,
) -> Element<'a, Message> {
    use crate::views::theme;
    use iced::widget::container as container_widget;

    let width = (state.window_width - 32.0).clamp(300.0, 640.0);
    let results = crate::commands::command_palette_results(state);
    let selected_index = if results.is_empty() {
        0
    } else {
        state.command_palette_selected.min(results.len() - 1)
    };

    let input = text_input(
        &tr("command-palette-placeholder"),
        &state.command_palette_query,
    )
    .id(crate::app::ids::command_palette_input_id())
    .on_input(Message::CommandPaletteInputChanged)
    .padding(10)
    .size(14);

    let mut result_list = Column::new().spacing(2);
    if results.is_empty() {
        result_list = result_list.push(
            container(
                text(tr("command-palette-no-results"))
                    .size(12)
                    .color(theme::muted_color(&crate::app::theme(state))),
            )
            .padding([10, 8])
            .width(Length::Fill),
        );
    } else {
        for (index, item) in results
            .iter()
            .take(crate::commands::COMMAND_PALETTE_VISIBLE_LIMIT)
            .enumerate()
        {
            let selected = index == selected_index;
            let shortcut = item.shortcut.unwrap_or_default().to_string();
            let row: Row<'_, Message> = Row::new()
                .push(text(item.label.clone()).size(13).width(Length::Fill))
                .push(
                    text(shortcut)
                        .size(11)
                        .color(theme::muted_color(&crate::app::theme(state))),
                )
                .spacing(10)
                .align_y(iced::Alignment::Center);
            let item_row =
                container(row)
                    .padding([7, 8])
                    .width(Length::Fill)
                    .style(move |theme: &Theme| {
                        if selected {
                            let palette = theme.extended_palette();
                            container_widget::Style {
                                background: Some(iced::Background::Color(iced::Color {
                                    a: 0.18,
                                    ..palette.primary.base.color
                                })),
                                border: iced::Border {
                                    radius: 4.0.into(),
                                    ..Default::default()
                                },
                                ..Default::default()
                            }
                        } else {
                            container_widget::Style::default()
                        }
                    });
            result_list = result_list
                .push(mouse_area(item_row).on_press(Message::ExecuteCommandPaletteIndex(index)));
        }
    }

    let palette_content = container(
        Column::new()
            .push(input)
            .push(scrollable(result_list).height(Length::Fixed(302.0)))
            .spacing(8)
            .width(Length::Fill),
    )
    .style(theme::dialog_card_style)
    .padding(12)
    .width(Length::Fixed(width));

    let backdrop = mouse_area(
        container(
            iced::widget::Space::new()
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .style(theme::dialog_backdrop_style)
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .on_press(Message::CloseCommandPalette);

    iced::widget::stack![
        main,
        backdrop,
        container(palette_content).center(Length::Fill)
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn view_with_quick_look<'a>(
    state: &'a AppState,
    main: Element<'a, Message>,
) -> Element<'a, Message> {
    use crate::views::theme;

    let available_width = (state.window_width - 32.0).max(280.0);
    let available_height = (state.window_height - 32.0).max(240.0);
    let overlay_width = (state.window_width * 0.72)
        .clamp(320.0, 960.0)
        .min(available_width);
    let overlay_height = (state.window_height * 0.72)
        .clamp(260.0, 720.0)
        .min(available_height);

    let close_button = button(
        container(theme::icon("\u{E5CD}").size(18).center())
            .center_x(Length::Fixed(theme::TOOLBAR_ICON_BTN))
            .center_y(Length::Fixed(theme::TOOLBAR_ICON_BTN)),
    )
    .padding(0)
    .style(theme::toolbar_icon_button_style)
    .on_press(Message::CloseQuickLook);

    let header = Row::new()
        .push(
            container(
                iced::widget::Space::new()
                    .width(Length::Fill)
                    .height(Length::Fixed(theme::TOOLBAR_ICON_BTN)),
            )
            .width(Length::Fill),
        )
        .push(close_button)
        .height(Length::Fixed(theme::PANEL_HEADER_HEIGHT))
        .align_y(iced::Alignment::Center);

    let preview_body = container(preview::view_preview_panel(state))
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true);

    let overlay_content = container(
        Column::new()
            .push(header)
            .push(preview_body)
            .spacing(4)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .style(theme::dialog_card_style)
    .padding(8)
    .width(Length::Fixed(overlay_width))
    .height(Length::Fixed(overlay_height));

    let backdrop = mouse_area(
        container(
            iced::widget::Space::new()
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .style(theme::dialog_backdrop_style)
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .on_press(Message::CloseQuickLook);

    iced::widget::stack![
        main,
        backdrop,
        container(overlay_content).center(Length::Fill)
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn view_main_layout(state: &AppState) -> Element<'_, Message> {
    use crate::views::{resize_handle, sidebar, theme};

    let resolved = super::resolved_layout::ResolvedLayout::resolve(state);
    let preview_panel_height = state.preview_panel_height.clamp(
        theme::PREVIEW_PANEL_MIN_HEIGHT,
        (resolved.window_height - 240.0).max(theme::PREVIEW_PANEL_MIN_HEIGHT),
    );

    let title_bar = if cfg!(target_os = "macos") {
        Some(view_title_bar_controls(state))
    } else {
        None
    };
    let toolbar = if state.dual_pane_enabled {
        None
    } else {
        Some(view_toolbar(state))
    };
    let status_bar = view_status_bar(state);

    let is_resizing_sidebar = matches!(
        state.resize_state,
        Some(ResizeState {
            target: ResizeTarget::Sidebar,
            ..
        })
    );
    let is_resizing_right = matches!(
        state.resize_state,
        Some(ResizeState {
            target: ResizeTarget::RightPanel,
            ..
        })
    );
    let is_resizing_preview = matches!(
        state.resize_state,
        Some(ResizeState {
            target: ResizeTarget::PreviewPanel,
            ..
        })
    );
    let is_resizing_divider = matches!(
        state.resize_state,
        Some(ResizeState {
            target: ResizeTarget::PaneDivider,
            ..
        })
    );

    let body_area: Element<'_, Message> = if state.dual_pane_enabled {
        let left_ratio = (state.pane_split_ratio * 100.0) as u16;
        let right_ratio = ((1.0 - state.pane_split_ratio) * 100.0) as u16;

        let pane_row = Row::new()
            .push(
                container(view_pane_content(state, 0))
                    .width(Length::FillPortion(left_ratio))
                    .height(Length::Fill)
                    .style(if state.active_pane == 0 {
                        theme::active_pane_style
                    } else {
                        |_: &Theme| container::Style::default()
                    }),
            )
            .push(resize_handle::resize_handle(
                ResizeTarget::PaneDivider,
                is_resizing_divider,
            ))
            .push(
                container(view_pane_content(state, 1))
                    .width(Length::FillPortion(right_ratio))
                    .height(Length::Fill)
                    .style(if state.active_pane == 1 {
                        theme::active_pane_style
                    } else {
                        |_: &Theme| container::Style::default()
                    }),
            )
            .height(Length::Fill);

        Column::new()
            .push(pane_row)
            .push(status_bar)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        let mut column = Column::new();
        if let Some(toolbar) = toolbar {
            column = column.push(toolbar);
        }
        column
            .push(view_pane_content(state, 0))
            .push(status_bar)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    };

    let mut layout = Row::new().height(Length::Fill);

    if resolved.sidebar_inline {
        layout = layout.push(
            container(sidebar::view_sidebar(state))
                .style(theme::sunken_panel_style)
                .width(Length::Fixed(resolved.sidebar_width))
                .height(Length::Fill),
        );
        layout = layout.push(resize_handle::resize_handle(
            ResizeTarget::Sidebar,
            is_resizing_sidebar,
        ));
    }

    let show_right_panel = resolved.right_panel_inline
        && (state.preview_panel_visible || state.properties_panel_visible);
    let use_positional_layout = show_right_panel && state.preview_panel_visible;

    match state.preview_position {
        PreviewPosition::Left if use_positional_layout => {
            let main_area: Element<'_, Message> = if let Some(title_bar) = title_bar {
                Column::new()
                    .push(title_bar)
                    .push(body_area)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            } else {
                body_area
            };

            layout = layout.push(
                container(build_right_panel_content(state))
                    .width(Length::Fixed(resolved.right_panel_width))
                    .height(Length::Fill),
            );
            layout = layout.push(resize_handle::resize_handle(
                ResizeTarget::RightPanel,
                is_resizing_right,
            ));
            layout = layout.push(
                container(main_area)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .clip(true),
            );
        }
        PreviewPosition::Top | PreviewPosition::Bottom if use_positional_layout => {
            let preview_content = build_right_panel_content(state);
            let main_with_preview = if state.preview_position == PreviewPosition::Top {
                if let Some(title_bar) = title_bar {
                    Column::new()
                        .push(title_bar)
                        .push(
                            container(preview_content)
                                .height(Length::Fixed(preview_panel_height))
                                .width(Length::Fill),
                        )
                        .push(resize_handle::resize_handle(
                            ResizeTarget::PreviewPanel,
                            is_resizing_preview,
                        ))
                        .push(body_area)
                        .height(Length::Fill)
                } else {
                    Column::new()
                        .push(
                            container(preview_content)
                                .height(Length::Fixed(preview_panel_height))
                                .width(Length::Fill),
                        )
                        .push(resize_handle::resize_handle(
                            ResizeTarget::PreviewPanel,
                            is_resizing_preview,
                        ))
                        .push(body_area)
                        .height(Length::Fill)
                }
            } else if let Some(title_bar) = title_bar {
                let main_area = Column::new()
                    .push(title_bar)
                    .push(body_area)
                    .width(Length::Fill)
                    .height(Length::Fill);
                Column::new()
                    .push(main_area)
                    .push(resize_handle::resize_handle(
                        ResizeTarget::PreviewPanel,
                        is_resizing_preview,
                    ))
                    .push(
                        container(preview_content)
                            .height(Length::Fixed(preview_panel_height))
                            .width(Length::Fill),
                    )
                    .height(Length::Fill)
            } else {
                Column::new()
                    .push(body_area)
                    .push(resize_handle::resize_handle(
                        ResizeTarget::PreviewPanel,
                        is_resizing_preview,
                    ))
                    .push(
                        container(preview_content)
                            .height(Length::Fixed(preview_panel_height))
                            .width(Length::Fill),
                    )
                    .height(Length::Fill)
            };
            layout = layout.push(
                container(main_with_preview)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .clip(true),
            );
        }
        _ => {
            let main_area: Element<'_, Message> = if let Some(title_bar) = title_bar {
                Column::new()
                    .push(title_bar)
                    .push(body_area)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            } else {
                body_area
            };
            layout = layout.push(
                container(main_area)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .clip(true),
            );

            if show_right_panel {
                layout = layout.push(resize_handle::resize_handle(
                    ResizeTarget::RightPanel,
                    is_resizing_right,
                ));
                layout = layout.push(
                    container(build_right_panel_content(state))
                        .width(Length::Fixed(resolved.right_panel_width))
                        .height(Length::Fill),
                );
            }
        }
    }

    let mut main: Element<'_, Message> = container(layout)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();

    if resolved.sidebar_overlay && !state.responsive_sidebar_dismissed {
        let header = row![
            text(tr("toolbar-sidebar")).size(12).width(Length::Fill),
            button(text("×").size(16))
                .padding([2, 8])
                .style(button::text)
                .on_press(Message::DismissResponsiveSidebar),
        ]
        .align_y(iced::Alignment::Center);
        let drawer = container(
            column![header, rule::horizontal(1), sidebar::view_sidebar(state)]
                .spacing(4)
                .height(Length::Fill),
        )
        .style(theme::sunken_panel_style)
        .padding([4, 8])
        .width(Length::Fixed(resolved.sidebar_width))
        .height(Length::Fill);
        main = stack![main, row![drawer, space::horizontal()].height(Length::Fill)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }

    if resolved.right_panel_overlay && !state.responsive_right_panel_dismissed {
        let header = row![
            text(tr("preview-title")).size(12).width(Length::Fill),
            button(text("×").size(16))
                .padding([2, 8])
                .style(button::text)
                .on_press(Message::DismissResponsiveRightPanel),
        ]
        .align_y(iced::Alignment::Center);
        let drawer = container(
            column![
                header,
                rule::horizontal(1),
                build_right_panel_content(state)
            ]
            .spacing(4)
            .height(Length::Fill),
        )
        .style(theme::panel_style)
        .padding([4, 8])
        .width(Length::Fixed(resolved.right_panel_width))
        .height(Length::Fill);
        main = stack![main, row![space::horizontal(), drawer].height(Length::Fill)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }

    if state.notifications.is_empty() {
        main
    } else {
        iced::widget::stack![
            main,
            notifications::view_notifications(&state.notifications)
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

fn build_right_panel_content(state: &AppState) -> Element<'_, Message> {
    use crate::views::{properties, theme};

    let both_visible = state.preview_panel_visible && state.properties_panel_visible;

    if both_visible {
        let preview_portion = (state.properties_split_ratio * 100.0) as u16;
        let props_portion = 100 - preview_portion;

        let preview_elem = container(preview::view_preview_panel(state))
            .width(Length::Fill)
            .height(Length::Fill);
        let props_elem = container(properties::view_properties_panel(state))
            .width(Length::Fill)
            .height(Length::Fill);

        let position = state.properties_position;
        let split_available_width = match state.preview_position {
            PreviewPosition::Top | PreviewPosition::Bottom => {
                super::resolved_layout::ResolvedLayout::resolve(state).main_width
            }
            PreviewPosition::Left | PreviewPosition::Right => {
                super::resolved_layout::ResolvedLayout::resolve(state).right_panel_width
            }
        };
        let use_horizontal_split = position.is_horizontal()
            && split_available_width >= theme::RIGHT_PANEL_HORIZONTAL_SPLIT_MIN_WIDTH;
        let resize_handle: Element<'_, Message> = if use_horizontal_split {
            mouse_area(
                container(iced::widget::rule::vertical(1))
                    .width(Length::Fixed(theme::RESIZE_HANDLE_WIDTH))
                    .height(Length::Fill)
                    .center_x(Length::Fixed(theme::RESIZE_HANDLE_WIDTH)),
            )
            .on_press(Message::ResizeStart(ResizeTarget::PropertiesDivider))
            .into()
        } else {
            mouse_area(
                container(iced::widget::rule::horizontal(1))
                    .height(Length::Fixed(theme::RESIZE_HANDLE_WIDTH))
                    .width(Length::Fill)
                    .center_y(Length::Fixed(theme::RESIZE_HANDLE_WIDTH)),
            )
            .on_press(Message::ResizeStart(ResizeTarget::PropertiesDivider))
            .into()
        };

        let (first_elem, first_portion, second_elem, second_portion) =
            if position.properties_first() {
                (props_elem, props_portion, preview_elem, preview_portion)
            } else {
                (preview_elem, preview_portion, props_elem, props_portion)
            };

        let content: Element<'_, Message> = if use_horizontal_split {
            Row::new()
                .push(
                    container(first_elem)
                        .width(Length::FillPortion(first_portion))
                        .height(Length::Fill),
                )
                .push(resize_handle)
                .push(
                    container(second_elem)
                        .width(Length::FillPortion(second_portion))
                        .height(Length::Fill),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            Column::new()
                .push(
                    container(first_elem)
                        .height(Length::FillPortion(first_portion))
                        .width(Length::Fill),
                )
                .push(resize_handle)
                .push(
                    container(second_elem)
                        .height(Length::FillPortion(second_portion))
                        .width(Length::Fill),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        };

        container(content)
            .style(theme::panel_style)
            .height(Length::Fill)
            .width(Length::Fill)
            .into()
    } else if state.preview_panel_visible {
        container(preview::view_preview_panel(state))
            .style(theme::panel_style)
            .height(Length::Fill)
            .width(Length::Fill)
            .into()
    } else if state.properties_panel_visible {
        container(properties::view_properties_panel(state))
            .style(theme::panel_style)
            .height(Length::Fill)
            .width(Length::Fill)
            .into()
    } else {
        container(Column::new())
            .style(theme::panel_style)
            .height(Length::Fill)
            .width(Length::Fill)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::local_entry;

    #[test]
    fn system_theme_uses_cached_appearance_with_iris_palette() {
        let mut state = AppState::default();
        state.theme_choice = ThemeChoice::System;
        state.system_theme = Theme::Light;

        assert_eq!(theme(&state), crate::views::theme::iris_light());

        state.system_theme = Theme::Dark;
        assert_eq!(theme(&state), crate::views::theme::iris_dark());
    }

    #[test]
    fn explicit_theme_choice_ignores_cached_system_theme() {
        let mut state = AppState::default();
        state.theme_choice = ThemeChoice::Dark;
        state.system_theme = Theme::Light;

        assert_eq!(theme(&state), crate::views::theme::iris_dark());
    }

    #[test]
    fn drag_ghost_data_uses_first_dragged_item_and_count() {
        let mut state = AppState::default();
        state.pane_mut(0).entries = vec![
            local_entry("/tmp/alpha.txt", false),
            local_entry("/tmp/beta.txt", false),
        ];
        state.dnd_state = DndState::Dragging {
            source_pane: 0,
            source_indices: vec![0, 1],
            target: None,
        };

        let data = drag_ghost_data(&state).expect("drag ghost");

        assert_eq!(data.title, "alpha.txt");
        assert!(data.count_label.contains('2'));
    }
}
