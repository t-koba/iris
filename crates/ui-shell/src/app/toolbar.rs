use crate::format::{format_size, truncate_with_ellipsis};
use crate::i18n::{tr, tr_with};
use crate::input::dnd::DndState;
use crate::messages::{Message, NavigationMessage, PaneMessage, SidebarMessage};
use crate::runtime::app_runtime;
use crate::state::{AppState, ClipboardContent, SortKey, SortOrder, ViewMode};
use crate::update::file_ops::current_dnd_operation;
use crate::update::helpers::{is_compact_layout, is_ultra_compact_layout, pane_available_width};
use iced::widget::{button, container, pick_list, row, space, text};
use iced::{Element, Length};

fn toolbar_icon_btn(label: &str, message: Option<Message>) -> iced::widget::Button<'_, Message> {
    use crate::views::theme;

    let button = button(
        container(theme::icon(label).size(16).center())
            .center_x(Length::Fixed(theme::TOOLBAR_ICON_BTN))
            .center_y(Length::Fixed(theme::TOOLBAR_ICON_BTN)),
    )
    .padding(0)
    .style(theme::toolbar_icon_button_style);
    match message {
        Some(message) => button.on_press(message),
        None => button,
    }
}

pub(super) fn toolbar_icon_with_tip<'a>(
    label: &'a str,
    tip: impl Into<String>,
    message: Option<Message>,
) -> Element<'a, Message> {
    use crate::views::theme;
    use iced::widget::tooltip;

    tooltip(
        toolbar_icon_btn(label, message),
        container(text(tip.into()).size(11))
            .padding([4, 8])
            .style(theme::tooltip_style),
        tooltip::Position::Bottom,
    )
    .gap(2)
    .snap_within_viewport(true)
    .into()
}

pub(super) fn view_title_bar_controls(state: &AppState) -> Element<'_, Message> {
    use crate::views::theme;

    let compact = is_compact_layout(state);
    let active_path = &state.active_pane().current_path;
    let title_label = if is_ultra_compact_layout(state) {
        String::new()
    } else if active_path.is_empty() {
        "Iris".to_string()
    } else {
        truncate_with_ellipsis(
            &app_runtime().format_display_path(active_path),
            if compact { 28 } else { 52 },
        )
    };

    let drag_area = iced::widget::mouse_area(
        container(text(title_label).size(12))
            .width(Length::Fill)
            .center_x(Length::Fill),
    )
    .on_press(Message::BeginWindowDrag);

    let left_controls = if state.sidebar_visible {
        row![].align_y(iced::Alignment::Center)
    } else {
        row![
            space::horizontal().width(Length::Fixed(76.0)),
            toolbar_icon_with_tip(
                "\u{E5D2}",
                tr("toolbar-sidebar"),
                Some(SidebarMessage::ToggleVisibility.into()),
            ),
        ]
        .align_y(iced::Alignment::Center)
    };

    let right_controls = row![
        toolbar_icon_with_tip(
            if state.dual_pane_enabled {
                "\u{E8EC}"
            } else {
                "\u{F06D}"
            },
            tr("toolbar-dual-pane"),
            Some(Message::ToggleDualPane),
        ),
        toolbar_icon_with_tip(
            if state.preview_panel_visible {
                "\u{F1C5}"
            } else {
                "\u{E8F5}"
            },
            tr("toolbar-preview"),
            Some(Message::TogglePreviewPanel),
        ),
        toolbar_icon_with_tip(
            "\u{E88E}",
            tr("toolbar-properties"),
            Some(Message::TogglePropertiesPanel)
        ),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center);

    container(
        row![left_controls, drag_area, right_controls]
            .spacing(theme::GROUP_SPACING)
            .align_y(iced::Alignment::Center),
    )
    .style(theme::toolbar_style)
    .padding([2, theme::ZONE_PADDING as u16])
    .width(Length::Fill)
    .height(Length::Fixed(theme::TOOLBAR_HEIGHT))
    .into()
}

pub(super) fn view_toolbar(state: &AppState) -> Element<'_, Message> {
    use crate::views::theme;

    let active_pane = state.active_pane();
    let navigation_group = row![
        toolbar_icon_with_tip(
            "\u{E5C4}",
            tr("toolbar-back"),
            if active_pane.can_go_back() {
                Some(NavigationMessage::Back.into())
            } else {
                None
            }
        ),
        toolbar_icon_with_tip(
            "\u{E5C8}",
            tr("toolbar-forward"),
            if active_pane.can_go_forward() {
                Some(NavigationMessage::Forward.into())
            } else {
                None
            }
        ),
        toolbar_icon_with_tip(
            "\u{E5D8}",
            tr("toolbar-up"),
            Some(NavigationMessage::Up.into())
        ),
        toolbar_icon_with_tip("\u{E5D5}", tr("toolbar-refresh"), Some(Message::Refresh)),
    ]
    .spacing(2)
    .align_y(iced::Alignment::Center);

    let active_selected = &active_pane.selected_indices;
    let availability = crate::commands::ActionAvailability::for_state(state);
    let has_selection = availability.has_selection;
    let has_single = availability.has_single_selection;
    let single_index = if has_single {
        active_selected.iter().copied().next()
    } else {
        None
    };

    let action_group = row![
        toolbar_icon_with_tip(
            "\u{E14D}",
            tr("common-copy"),
            if has_selection {
                Some(Message::CopySelected)
            } else {
                None
            }
        ),
        toolbar_icon_with_tip(
            "\u{E14E}",
            tr("common-cut"),
            if has_selection && availability.can_mutate {
                Some(Message::CutSelected)
            } else {
                None
            }
        ),
        toolbar_icon_with_tip(
            "\u{E14F}",
            tr("toolbar-paste"),
            if availability.can_paste {
                Some(Message::Paste)
            } else {
                None
            }
        ),
        toolbar_icon_with_tip(
            "\u{E9A2}",
            tr("common-rename"),
            if has_single && availability.can_mutate {
                single_index.map(Message::Rename)
            } else if has_selection && availability.can_mutate {
                Some(Message::BatchRenameSelected)
            } else {
                None
            }
        ),
        toolbar_icon_with_tip(
            "\u{E92E}",
            tr("common-delete"),
            if has_selection && availability.can_mutate {
                Some(Message::DeleteSelected)
            } else {
                None
            }
        ),
        toolbar_icon_with_tip(
            "\u{E2CC}",
            tr("toolbar-new-folder"),
            availability
                .can_create_folder
                .then_some(Message::CreateFolder)
        ),
    ]
    .spacing(2)
    .align_y(iced::Alignment::Center);

    let active_pane = state.active_pane();
    let active_sort_key = active_pane.sort_key;
    let active_sort_order = &active_pane.sort_order;
    let active_view_mode = &active_pane.view_mode;

    let pane_index = state.active_pane;
    let sort_picker = pick_list(SortKey::ALL.as_slice(), Some(active_sort_key), move |key| {
        Message::Pane(pane_index, PaneMessage::SetSortKey(key))
    })
    .text_size(11)
    .padding([4, 6]);

    let sort_order_message = Message::Pane(pane_index, PaneMessage::ToggleSortOrder);
    let view_toggle_message = Message::Pane(
        pane_index,
        PaneMessage::SetViewMode(match state.active_pane().view_mode {
            ViewMode::Grid => ViewMode::List,
            ViewMode::List => ViewMode::Grid,
        }),
    );

    let view_group = row![
        sort_picker,
        toolbar_icon_with_tip(
            if *active_sort_order == SortOrder::Ascending {
                "\u{E5D8}"
            } else {
                "\u{E5DB}"
            },
            tr("toolbar-sort-order"),
            Some(sort_order_message),
        ),
        toolbar_icon_with_tip(
            match active_view_mode {
                ViewMode::Grid => "\u{E8EF}",
                ViewMode::List => "\u{E9B0}",
            },
            tr("toolbar-view-mode"),
            Some(view_toggle_message),
        ),
        toolbar_icon_with_tip(
            if state.show_hidden_files {
                "\u{E8F4}"
            } else {
                "\u{E8F5}"
            },
            tr("toolbar-hidden-files"),
            Some(Message::ToggleHiddenFiles),
        ),
    ]
    .spacing(2)
    .align_y(iced::Alignment::Center);

    let toolbar_width = pane_available_width(state, state.active_pane);
    toolbar_wrap(
        navigation_group.into(),
        action_group.into(),
        view_group.into(),
        theme::GROUP_SPACING,
        [2, theme::ZONE_PADDING as u16],
        toolbar_width,
    )
}

pub(super) fn toolbar_wrap<'a>(
    primary: Element<'a, Message>,
    secondary: Element<'a, Message>,
    trailing: Element<'a, Message>,
    spacing: f32,
    padding: [u16; 2],
    toolbar_width: f32,
) -> Element<'a, Message> {
    use crate::views::theme;

    let available = toolbar_width - padding[1] as f32 * 2.0;

    let content: Element<'_, Message> = if available >= 720.0 {
        row![primary, secondary, space::horizontal(), trailing]
            .spacing(spacing)
            .align_y(iced::Alignment::Center)
            .width(Length::Fill)
            .height(Length::Shrink)
            .into()
    } else if available >= 420.0 {
        let top = row![primary, secondary]
            .spacing(spacing)
            .align_y(iced::Alignment::Center)
            .width(Length::Fill);
        let bottom = row![space::horizontal(), trailing]
            .align_y(iced::Alignment::Center)
            .width(Length::Fill);
        iced::widget::column![top, bottom]
            .spacing(2)
            .width(Length::Fill)
            .into()
    } else {
        let primary_row = row![primary]
            .align_y(iced::Alignment::Center)
            .width(Length::Fill);
        let secondary_row = row![secondary]
            .align_y(iced::Alignment::Center)
            .width(Length::Fill)
            .height(Length::Shrink);
        let trailing_row = row![trailing]
            .align_y(iced::Alignment::Center)
            .width(Length::Fill);
        iced::widget::column![primary_row, secondary_row, trailing_row]
            .spacing(2)
            .width(Length::Fill)
            .into()
    };

    container(content)
        .style(theme::toolbar_style)
        .padding(padding)
        .width(Length::Fill)
        .height(Length::Shrink)
        .into()
}

pub(super) fn view_status_bar(state: &AppState) -> Element<'_, Message> {
    use crate::views::theme;

    let muted = theme::muted_color(&crate::app::theme(state));
    let active = state.active_pane();
    let (count, selected) = (active.entries.len(), active.selected_indices.len());
    let mut parts = vec![tr_with("status-items", &[("count", count.into())])];
    if selected > 0 {
        let total_size: u64 = active
            .selected_indices
            .iter()
            .filter_map(|&index| active.entry(index))
            .filter_map(|entry| entry.size)
            .sum();
        if total_size > 0 {
            parts.push(tr_with(
                "status-selected-size",
                &[
                    ("count", selected.into()),
                    ("size", format_size(total_size).into()),
                ],
            ));
        } else {
            parts.push(tr_with("status-selected", &[("count", selected.into())]));
        }
    }
    match &state.clipboard {
        Some(ClipboardContent::Copy(entries)) => parts.push(format!("CP:{}", entries.len())),
        Some(ClipboardContent::Cut(entries)) => parts.push(format!("CUT:{}", entries.len())),
        None => {}
    }
    match &state.dnd_state {
        DndState::Pressed { source_indices, .. } | DndState::Dragging { source_indices, .. } => {
            let mut label = format!("DnD:{}", source_indices.len());
            if let Some(operation) = current_dnd_operation(state) {
                label.push_str(match operation {
                    crate::input::dnd::DndOperation::Copy => " copy",
                    crate::input::dnd::DndOperation::Move => " move",
                    crate::input::dnd::DndOperation::Link => " link",
                });
            }
            parts.push(label);
        }
        DndState::ExternalDragging { source_paths, .. } => {
            let mut label = format!("DnD:{}", source_paths.len());
            if let Some(operation) = current_dnd_operation(state) {
                label.push_str(match operation {
                    crate::input::dnd::DndOperation::Copy => " copy",
                    crate::input::dnd::DndOperation::Move => " move",
                    crate::input::dnd::DndOperation::Link => " link",
                });
            }
            parts.push(label);
        }
        DndState::Idle => {}
    }

    let left = text(parts.join(" | ")).size(11).color(muted);

    let focused_entry = active.focused_index.and_then(|index| active.entry(index));
    let right: Element<'_, Message> = if let Some(entry) = focused_entry {
        if entry.is_dir {
            if let Some(stats) = &state.focused_folder_stats {
                let mut info = vec![entry.name.clone()];
                if stats.file_count > 0 || stats.dir_count > 0 {
                    let mut parts = Vec::new();
                    if stats.dir_count > 0 {
                        parts.push(tr_with(
                            "status-folders",
                            &[("count", stats.dir_count.into())],
                        ));
                    }
                    if stats.file_count > 0 {
                        parts.push(tr_with(
                            "status-files",
                            &[("count", stats.file_count.into())],
                        ));
                    }
                    if stats.total_size > 0 {
                        parts.push(format_size(stats.total_size));
                    }
                    info.push(format!("({})", parts.join(", ")));
                }
                text(truncate_with_ellipsis(&info.join(" "), 56))
                    .size(11)
                    .into()
            } else {
                text(truncate_with_ellipsis(&entry.name, 56))
                    .size(11)
                    .into()
            }
        } else {
            let size = entry.size.map(format_size).unwrap_or_default();
            if size.is_empty() {
                text(truncate_with_ellipsis(&entry.name, 56))
                    .size(11)
                    .into()
            } else {
                text(truncate_with_ellipsis(
                    &format!("{} ({})", entry.name, size),
                    56,
                ))
                .size(11)
                .into()
            }
        }
    } else {
        text("").size(11).into()
    };

    container(
        row![left, space::horizontal(), right]
            .align_y(iced::Alignment::Center)
            .height(Length::Shrink),
    )
    .style(theme::status_bar_style)
    .padding([3, theme::ZONE_PADDING as u16])
    .width(Length::Fill)
    .height(Length::Fixed(theme::STATUS_BAR_HEIGHT))
    .into()
}
