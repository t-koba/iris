use crate::format::{format_size, format_time, truncate_with_ellipsis};
use crate::i18n::tr;
use crate::input::dnd::DndTarget;
use crate::messages::{Message, PaneMessage};
use crate::state::{
    AppState, EntryIndex, ListColumnKind, ListColumnSpec, PaneState, ResizeTarget, SortKey,
    SortOrder,
};
use crate::update::helpers::grid_items_per_row_for_pane;
use crate::widgets::metrics::{
    GRID_FILE_ROW_HEIGHT, GRID_FOLDER_ROW_HEIGHT, GRID_SECTION_SEPARATOR_HEIGHT,
    LIST_HEADER_HEIGHT, LIST_ROW_HEIGHT,
};
use crate::widgets::virtual_grid::VirtualGrid;
use crate::widgets::virtual_list::VirtualList;
use iced::widget::{button, column, container, mouse_area, text, text_input, Column, Row, Space};
use iced::{Color, Element, Length, Theme};
use iris_core_domain::types::list::ListEntry;

const LIST_WINDOW_OVERSCAN_ROWS: usize = 12;
const GRID_WINDOW_OVERSCAN_ROWS: usize = 4;

fn velocity_overscan(base: usize, velocity_y: f32) -> usize {
    base.saturating_add((velocity_y.abs() / 600.0) as usize)
        .min(base * 6)
}

fn list_col_resize_handle<'a>(target: ResizeTarget) -> Element<'a, Message> {
    use crate::views::theme;

    mouse_area(
        container(
            container(text(""))
                .width(Length::Fixed(1.0))
                .height(Length::Fill)
                .style(move |_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(iced::Color::from_rgba(
                        0.5, 0.5, 0.5, 0.3,
                    ))),
                    ..Default::default()
                }),
        )
        .width(Length::Fixed(theme::LIST_COL_RESIZE_HANDLE))
        .height(Length::Fixed(22.0))
        .center_x(Length::Fixed(theme::LIST_COL_RESIZE_HANDLE)),
    )
    .on_press(Message::ResizeStart(target))
    .into()
}

pub(super) fn view_pane_list(
    state: &AppState,
    pane_idx: usize,
    compact: bool,
) -> Element<'_, Message> {
    let pane = state.pane(pane_idx);
    let visible_columns = pane
        .visible_list_columns(compact)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let pane_index = pane_idx;
    VirtualList::lazy(
        crate::app::ids::pane_scroll_id(pane_idx),
        pane.scroll_y,
        pane.viewport_height,
        LIST_HEADER_HEIGHT + pane.list_rows_height(),
        move |offset_y, viewport_height| {
            build_list_rows(
                state,
                pane_idx,
                compact,
                &visible_columns,
                offset_y,
                viewport_height,
            )
        },
    )
    .on_scroll(move |offset_y, viewport_height| {
        Message::Pane(
            pane_index,
            PaneMessage::Scrolled {
                offset_y,
                viewport_height,
            },
        )
    })
    .into()
}

fn build_list_rows<'a>(
    state: &'a AppState,
    pane_idx: usize,
    compact: bool,
    visible_columns: &[ListColumnSpec],
    offset_y: f32,
    viewport_height: f32,
) -> Vec<(u64, f32, Element<'a, Message>)> {
    use crate::views::theme;

    let pane = state.pane(pane_idx);
    let active_theme = crate::app::theme(state);
    let muted = theme::muted_color(&active_theme);
    let mut items = Vec::new();
    let overscan = velocity_overscan(LIST_WINDOW_OVERSCAN_ROWS, pane.scroll_velocity_y);
    if offset_y <= LIST_HEADER_HEIGHT + overscan as f32 * LIST_ROW_HEIGHT {
        let sort_key = pane.sort_key;
        let sort_order = pane.sort_order;
        let sort_indicator = |key: SortKey| -> &'static str {
            if sort_key == key {
                if sort_order == SortOrder::Ascending {
                    " ↑"
                } else {
                    " ↓"
                }
            } else {
                ""
            }
        };

        let mut header = Row::new().spacing(0).align_y(iced::Alignment::Center);
        header = header.push(iced::widget::Space::new().width(theme::LIST_ICON_COL + 4.0));
        for (position, column) in visible_columns.iter().enumerate() {
            if position > 0 {
                header = header.push(list_col_resize_handle(ResizeTarget::ListColumnBoundary {
                    pane: pane_idx,
                    left: visible_columns[position - 1].kind,
                    right: column.kind,
                }));
            }
            let column_sort_key = column.kind.sort_key();
            let label = if column.sortable {
                format!(
                    "{}{}",
                    tr(column.kind.i18n_key()),
                    sort_indicator(column_sort_key)
                )
            } else {
                tr(column.kind.i18n_key())
            };
            let mut header_button = button(text(label).size(11))
                .padding([2, 4])
                .style(button::text)
                .width(list_column_length(column));
            if column.sortable {
                header_button = header_button.on_press(Message::Pane(
                    pane_idx,
                    PaneMessage::SetSortKey(column_sort_key),
                ));
            }
            header = header.push(header_button);
        }
        items.push((
            u64::MAX,
            0.0,
            container(header)
                .style(theme::list_header_style)
                .padding([2, 8])
                .width(Length::Fill)
                .into(),
        ));
    }

    let visible_indices = pane.visible_indices();
    let window = list_window_at(
        pane,
        state.window_height,
        visible_indices.len(),
        offset_y,
        viewport_height,
    );
    for position in window.start..window.end {
        let Some(index) = visible_indices.get(position).copied() else {
            continue;
        };
        let Some(entry) = pane.entry(index) else {
            continue;
        };
        items.push((
            index as u64,
            LIST_HEADER_HEIGHT + pane.list_row_offset(position),
            view_list_entry(ListEntryViewContext {
                state,
                pane_idx,
                index,
                entry,
                compact,
                muted,
                columns: visible_columns,
                visible_position: position,
            }),
        ));
    }
    items
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ListWindow {
    start: usize,
    end: usize,
    top_spacer: f32,
    bottom_spacer: f32,
}

#[cfg(test)]
fn list_window(pane: &PaneState, window_height: f32, total_visible: usize) -> ListWindow {
    list_window_at(
        pane,
        window_height,
        total_visible,
        pane.scroll_y,
        pane.viewport_height,
    )
}

fn list_window_at(
    pane: &PaneState,
    window_height: f32,
    total_visible: usize,
    scroll_y: f32,
    measured_viewport_height: f32,
) -> ListWindow {
    if total_visible == 0 {
        return ListWindow {
            start: 0,
            end: 0,
            top_spacer: 0.0,
            bottom_spacer: 0.0,
        };
    }

    let viewport_height = if measured_viewport_height > 1.0 {
        measured_viewport_height
    } else {
        (window_height - 180.0).max(LIST_ROW_HEIGHT)
    };
    let row_scroll = (scroll_y.max(0.0) - LIST_HEADER_HEIGHT).max(0.0);
    let first_visible = pane
        .visible_list_row_offsets
        .partition_point(|offset| *offset <= row_scroll)
        .saturating_sub(1)
        .min(total_visible.saturating_sub(1));
    let visible_end = pane
        .visible_list_row_offsets
        .partition_point(|offset| *offset < row_scroll + viewport_height)
        .min(total_visible);
    let overscan = velocity_overscan(LIST_WINDOW_OVERSCAN_ROWS, pane.scroll_velocity_y);
    let start = first_visible
        .saturating_sub(overscan)
        .min(total_visible.saturating_sub(1));
    let end = visible_end.saturating_add(overscan).min(total_visible);

    ListWindow {
        start,
        end: end.max(start + 1).min(total_visible),
        top_spacer: pane.list_row_offset(start),
        bottom_spacer: pane.list_rows_height() - pane.list_row_offset(end),
    }
}

struct ListEntryViewContext<'a, 'b> {
    state: &'a AppState,
    pane_idx: usize,
    index: EntryIndex,
    entry: &'a ListEntry,
    compact: bool,
    muted: Color,
    columns: &'b [ListColumnSpec],
    visible_position: usize,
}

fn view_list_entry<'a>(context: ListEntryViewContext<'a, '_>) -> Element<'a, Message> {
    use crate::views::theme;

    let ListEntryViewContext {
        state,
        pane_idx,
        index,
        entry,
        compact,
        muted,
        columns,
        visible_position,
    } = context;

    let pane = state.pane(pane_idx);
    let is_selected = pane.selected_indices.contains(&index);
    let is_focused = pane.focused_index == Some(index);
    let is_drop_target = entry.is_dir
        && state.dnd_state.target()
            == Some(DndTarget::Entry {
                pane: pane_idx,
                index,
            });

    let icon_element: Element<'_, Message> = if entry.is_inaccessible {
        theme::icon("\u{E899}")
            .size(16)
            .color(Color {
                r: 0.55,
                g: 0.35,
                b: 0.35,
                a: 1.0,
            })
            .into()
    } else {
        theme::file_icon_ex(entry.is_dir, entry.is_symlink, &crate::app::theme(state))
    };

    let mut entry_row = Row::new().spacing(4).align_y(iced::Alignment::Center);
    entry_row = entry_row.push(
        container(icon_element)
            .width(Length::Fixed(theme::LIST_ICON_COL))
            .height(Length::Fixed(theme::LIST_ICON_COL))
            .center_x(Length::Fixed(theme::LIST_ICON_COL))
            .center_y(Length::Fixed(theme::LIST_ICON_COL)),
    );
    let inline_rename = pane.inline_rename_for(index);
    let inline_rename_input =
        inline_rename.map(|rename| (rename.input_value.as_str(), rename.error_message.as_deref()));
    let cell_context = ListColumnCellContext {
        pane_idx,
        index,
        entry,
        pane,
        inline_rename: inline_rename_input,
        compact,
        muted,
    };
    for (position, column) in columns.iter().enumerate() {
        if position > 0 {
            entry_row =
                entry_row.push(Space::new().width(Length::Fixed(theme::LIST_COL_RESIZE_HANDLE)));
        }
        entry_row = entry_row.push(view_list_column_cell(&cell_context, column));
    }

    if inline_rename.is_some() {
        let entry_item: Element<'_, Message> = container(entry_row)
            .width(Length::Fill)
            .padding([theme::LIST_ROW_PADDING_V, theme::LIST_ROW_PADDING_H])
            .into();
        if visible_position.is_multiple_of(2) {
            return container(entry_item)
                .style(theme::list_row_alt_style)
                .width(Length::Fill)
                .into();
        }
        return entry_item;
    }

    let style = if is_drop_target {
        button::success
    } else if is_selected {
        button::primary
    } else if is_focused {
        button::secondary
    } else {
        button::text
    };

    let press_msg = if entry.is_inaccessible {
        None
    } else if entry.is_dir && pane_idx == 0 {
        Some(Message::Pane(pane_idx, PaneMessage::SelectItem(index)))
    } else if entry.is_dir {
        let path = entry.object_ref.display_path.clone();
        Some(Message::Pane(pane_idx, PaneMessage::NavigateToPath(path)))
    } else {
        Some(Message::Pane(pane_idx, PaneMessage::SelectItem(index)))
    };

    let entry_btn = if let Some(message) = press_msg {
        button(entry_row).on_press(message)
    } else {
        button(entry_row)
    }
    .width(Length::Fill)
    .padding([theme::LIST_ROW_PADDING_V, theme::LIST_ROW_PADDING_H])
    .style(style);

    let entry_area = if entry.is_inaccessible {
        mouse_area(entry_btn).on_press(Message::SetActivePane(pane_idx))
    } else {
        let area = mouse_area(entry_btn).on_press(Message::DragStart {
            pane: pane_idx,
            index,
        });
        if entry.is_dir {
            area.on_enter(Message::DragHover(DndTarget::Entry {
                pane: pane_idx,
                index,
            }))
            .on_exit(Message::DragLeave(DndTarget::Entry {
                pane: pane_idx,
                index,
            }))
        } else {
            area
        }
    };

    let entry_item: Element<'_, Message> = entry_area
        .on_right_press(Message::OpenItemContextMenu {
            pane: pane_idx,
            index,
        })
        .into();

    if visible_position.is_multiple_of(2) {
        container(entry_item)
            .style(theme::list_row_alt_style)
            .width(Length::Fill)
            .into()
    } else {
        entry_item
    }
}

fn list_column_length(column: &ListColumnSpec) -> Length {
    if column.kind.is_flexible() {
        Length::Fill
    } else {
        Length::Fixed(column.width)
    }
}

struct ListColumnCellContext<'a> {
    pane_idx: usize,
    index: EntryIndex,
    entry: &'a ListEntry,
    pane: &'a PaneState,
    inline_rename: Option<(&'a str, Option<&'a str>)>,
    compact: bool,
    muted: Color,
}

fn view_list_column_cell<'a>(
    context: &ListColumnCellContext<'a>,
    column: &ListColumnSpec,
) -> Element<'a, Message> {
    match column.kind {
        ListColumnKind::Name => {
            if let Some((input_value, error_message)) = context.inline_rename {
                inline_rename_cell(
                    context.pane_idx,
                    context.index,
                    context.entry,
                    input_value,
                    error_message,
                )
            } else {
                list_name_cell(
                    context.entry,
                    &context.pane.current_path,
                    context.pane.filter_model.scope == iris_search::filter::Scope::Recursive,
                    context.compact,
                    context.muted,
                )
            }
        }
        ListColumnKind::Size => {
            let size_str = if context.entry.is_dir {
                "-".to_string()
            } else if context.entry.metadata_pending() {
                "...".to_string()
            } else {
                context.entry.size.map(format_size).unwrap_or_default()
            };
            text(size_str)
                .size(11)
                .width(list_column_length(column))
                .color(context.muted)
                .into()
        }
        ListColumnKind::Date => {
            let mtime_str = if context.entry.metadata_pending() {
                "...".to_string()
            } else {
                context
                    .entry
                    .mtime
                    .map(format_time)
                    .unwrap_or_else(|| "-".to_string())
            };
            text(mtime_str)
                .size(11)
                .width(list_column_length(column))
                .color(context.muted)
                .into()
        }
    }
}

fn inline_rename_cell<'a>(
    pane_idx: usize,
    index: EntryIndex,
    entry: &'a ListEntry,
    input_value: &'a str,
    error_message: Option<&'a str>,
) -> Element<'a, Message> {
    let input_id =
        crate::app::ids::inline_rename_input_id(pane_idx, &entry.object_ref.display_path);
    let mut cell = Column::new().spacing(2).width(Length::Fill).push(
        text_input(&entry.name, input_value)
            .id(input_id)
            .on_input(move |value| {
                Message::Pane(
                    pane_idx,
                    PaneMessage::InlineRenameInputChanged { index, value },
                )
            })
            .on_submit(Message::Pane(
                pane_idx,
                PaneMessage::CommitInlineRename(index),
            ))
            .size(12)
            .padding([2, 4])
            .width(Length::Fill),
    );
    if let Some(message) = error_message {
        cell = cell.push(text(message).size(10).color(Color {
            r: 0.85,
            g: 0.22,
            b: 0.22,
            a: 1.0,
        }));
    }
    cell.into()
}

fn list_name_cell<'a>(
    entry: &'a ListEntry,
    root_path: &str,
    show_relative_parent: bool,
    compact: bool,
    muted: Color,
) -> Element<'a, Message> {
    let display_name = truncate_with_ellipsis(&entry.name, if compact { 42 } else { 64 });
    let mut name_block = Column::new().spacing(0).width(Length::Fill);

    if entry.is_inaccessible {
        name_block = name_block.push(text(display_name).size(12).color(muted));
    } else if entry.is_symlink {
        let mut symlink_row = Row::new()
            .spacing(4)
            .align_y(iced::Alignment::Center)
            .push(text(display_name).size(12));
        if let Some(target) = &entry.symlink_target {
            symlink_row = symlink_row.push(
                text(truncate_with_ellipsis(&format!("-> {}", target), 42))
                    .size(10)
                    .color(muted),
            );
        }
        name_block = name_block.push(symlink_row.width(Length::Fill));
    } else {
        name_block = name_block.push(text(display_name).size(12));
    }

    if show_relative_parent {
        if let Some(relative_parent) = relative_parent_label(root_path, entry) {
            name_block = name_block.push(
                text(truncate_with_ellipsis(
                    &relative_parent,
                    if compact { 36 } else { 56 },
                ))
                .size(10)
                .color(muted),
            );
        }
    }

    if let Some(annotation) = entry
        .annotation
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        name_block = name_block.push(
            text(truncate_with_ellipsis(
                annotation,
                if compact { 44 } else { 84 },
            ))
            .size(10)
            .color(muted),
        );
    }

    name_block.into()
}

fn relative_parent_label(root_path: &str, entry: &ListEntry) -> Option<String> {
    let full_path = entry.object_ref.display_path.trim_end_matches('/');
    let root = root_path.trim_end_matches('/');
    if full_path.is_empty() || root.is_empty() {
        return None;
    }

    let relative = full_path
        .strip_prefix(root)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .unwrap_or(full_path);
    let parent = std::path::Path::new(relative)
        .parent()
        .and_then(|parent| parent.to_str())
        .filter(|parent| !parent.is_empty() && *parent != ".");

    parent.map(|value| value.replace('\\', "/"))
}

pub(super) fn view_pane_grid(state: &AppState, pane_idx: usize) -> Element<'_, Message> {
    let pane = state.pane(pane_idx);
    let items_per_row = grid_items_per_row_for_pane(state, pane_idx);
    let folder_row_count = pane.visible_folder_indices().len().div_ceil(items_per_row);
    let has_folders = !pane.visible_folder_indices().is_empty();
    let has_files = !pane.visible_file_indices().is_empty();
    let separator_height = if has_folders && has_files {
        GRID_SECTION_SEPARATOR_HEIGHT
    } else {
        0.0
    };
    let file_section_top = folder_row_count as f32 * GRID_FOLDER_ROW_HEIGHT + separator_height;
    let file_row_count = pane.visible_file_indices().len().div_ceil(items_per_row);
    let total_height = file_section_top + file_row_count as f32 * GRID_FILE_ROW_HEIGHT;
    let pane_index = pane_idx;

    container(
        VirtualGrid::lazy(
            crate::app::ids::pane_scroll_id(pane_idx),
            pane.scroll_y,
            pane.viewport_height,
            total_height,
            move |offset_y, viewport_height| {
                build_grid_rows(state, pane_idx, offset_y, viewport_height)
            },
        )
        .on_scroll(move |offset_y, viewport_height| {
            Message::Pane(
                pane_index,
                PaneMessage::GridScrolled {
                    offset_y,
                    viewport_height,
                },
            )
        }),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding([0, 8])
    .clip(true)
    .into()
}

fn build_grid_rows(
    state: &AppState,
    pane_idx: usize,
    offset_y: f32,
    viewport_height: f32,
) -> Vec<(u64, f32, Element<'_, Message>)> {
    use crate::views::theme;

    let pane = state.pane(pane_idx);
    let active_theme = crate::app::theme(state);
    let muted = theme::muted_color(&active_theme);
    let mut rows = Vec::<(u64, f32, Element<'_, Message>)>::new();
    let items_per_row = grid_items_per_row_for_pane(state, pane_idx);
    let folder_cell_est_width = 110.0_f32;
    let folders_per_row = items_per_row;

    let folders = pane.visible_folder_indices();
    let files = pane.visible_file_indices();

    let folder_row_count = folders.len().div_ceil(folders_per_row);
    let folder_window = fixed_row_window(
        offset_y,
        viewport_height,
        state.window_height,
        folder_row_count,
        GRID_FOLDER_ROW_HEIGHT,
        0.0,
        velocity_overscan(GRID_WINDOW_OVERSCAN_ROWS, pane.scroll_velocity_y),
    );

    if !folders.is_empty() {
        for row_index in folder_window.start..folder_window.end {
            let start = row_index * folders_per_row;
            let end = (start + folders_per_row).min(folders.len());
            let chunk = &folders[start..end];
            let mut row_widget = Row::new().spacing(2);
            for &index in chunk {
                let Some(entry) = pane.entry(index) else {
                    continue;
                };
                let is_selected = pane.selected_indices.contains(&index);
                let is_focused = pane.focused_index == Some(index);
                let is_drop_target = entry.is_dir
                    && state.dnd_state.target()
                        == Some(DndTarget::Entry {
                            pane: pane_idx,
                            index,
                        });

                let style = if is_drop_target {
                    button::success
                } else if is_selected {
                    button::primary
                } else if is_focused {
                    button::secondary
                } else {
                    button::text
                };

                let press_msg = if pane_idx == 0 {
                    Message::Pane(pane_idx, PaneMessage::SelectItem(index))
                } else {
                    let path = entry.object_ref.display_path.clone();
                    Message::Pane(pane_idx, PaneMessage::NavigateToPath(path))
                };

                let display_name = if entry.name.chars().count() > 16 {
                    truncate_with_ellipsis(&entry.name, 14)
                } else {
                    entry.name.clone()
                };
                let mut folder_content = iced::widget::row![
                    theme::icon("\u{E2C7}").size(16),
                    text(display_name).size(11).width(Length::Fill)
                ]
                .spacing(4);
                if entry.is_symlink {
                    folder_content =
                        folder_content.push(theme::icon("\u{E157}").size(10).color(muted));
                }
                let folder_content = folder_content.align_y(iced::Alignment::Center);

                let cell = mouse_area(
                    button(folder_content)
                        .on_press(press_msg)
                        .width(Length::Fixed(folder_cell_est_width))
                        .height(Length::Fixed(28.0))
                        .padding([2, 6])
                        .style(style),
                )
                .on_press(Message::DragStart {
                    pane: pane_idx,
                    index,
                })
                .on_enter(Message::DragHover(DndTarget::Entry {
                    pane: pane_idx,
                    index,
                }))
                .on_exit(Message::DragLeave(DndTarget::Entry {
                    pane: pane_idx,
                    index,
                }))
                .on_right_press(Message::OpenItemContextMenu {
                    pane: pane_idx,
                    index,
                });

                row_widget = row_widget.push(cell);
            }
            rows.push((
                0x1000_0000_0000_0000 | row_index as u64,
                row_index as f32 * GRID_FOLDER_ROW_HEIGHT,
                container(row_widget)
                    .height(Length::Fixed(GRID_FOLDER_ROW_HEIGHT))
                    .width(Length::Fill)
                    .into(),
            ));
        }

        if !files.is_empty() {
            rows.push((
                0x1800_0000_0000_0000,
                folder_row_count as f32 * GRID_FOLDER_ROW_HEIGHT,
                container(iced::widget::rule::horizontal(1))
                    .height(Length::Fixed(GRID_SECTION_SEPARATOR_HEIGHT))
                    .width(Length::Fill)
                    .into(),
            ));
        }
    }

    let separator_height = if !folders.is_empty() && !files.is_empty() {
        GRID_SECTION_SEPARATOR_HEIGHT
    } else {
        0.0
    };
    let file_section_top = folder_row_count as f32 * GRID_FOLDER_ROW_HEIGHT + separator_height;
    let file_row_count = files.len().div_ceil(items_per_row);
    let file_window = fixed_row_window(
        offset_y,
        viewport_height,
        state.window_height,
        file_row_count,
        GRID_FILE_ROW_HEIGHT,
        file_section_top,
        velocity_overscan(GRID_WINDOW_OVERSCAN_ROWS, pane.scroll_velocity_y),
    );

    for row_index in file_window.start..file_window.end {
        let start = row_index * items_per_row;
        let end = (start + items_per_row).min(files.len());
        let chunk = &files[start..end];
        let mut row_widget = Row::new().spacing(4);
        for &index in chunk {
            let Some(entry) = pane.entry(index) else {
                continue;
            };
            let is_selected = pane.selected_indices.contains(&index);
            let is_focused = pane.focused_index == Some(index);

            let style = if is_selected {
                button::primary
            } else if is_focused {
                button::secondary
            } else {
                button::text
            };

            let thumb_element: Element<'_, Message> = if entry.is_inaccessible {
                container(theme::icon("\u{E899}").size(36).color(Color {
                    r: 0.55,
                    g: 0.35,
                    b: 0.35,
                    a: 1.0,
                }))
                .width(Length::Fixed(theme::GRID_THUMB_SIZE))
                .height(Length::Fixed(theme::GRID_THUMB_SIZE))
                .center_x(Length::Fixed(theme::GRID_THUMB_SIZE))
                .center_y(Length::Fixed(theme::GRID_THUMB_SIZE))
                .into()
            } else if let Some(thumbnail) = state
                .thumbnail_cache
                .get(&crate::state::ThumbnailCacheKey::from_entry(entry))
            {
                iced::widget::image(thumbnail.handle.clone())
                    .width(Length::Fixed(theme::GRID_THUMB_SIZE))
                    .height(Length::Fixed(theme::GRID_THUMB_SIZE))
                    .content_fit(iced::ContentFit::Contain)
                    .into()
            } else {
                container(theme::icon("\u{E873}").size(36).color(Color {
                    r: 0.45,
                    g: 0.45,
                    b: 0.45,
                    a: 1.0,
                }))
                .width(Length::Fixed(theme::GRID_THUMB_SIZE))
                .height(Length::Fixed(theme::GRID_THUMB_SIZE))
                .center_x(Length::Fixed(theme::GRID_THUMB_SIZE))
                .center_y(Length::Fixed(theme::GRID_THUMB_SIZE))
                .into()
            };

            let display_name = if entry.name.chars().count() > 14 {
                truncate_with_ellipsis(&entry.name, 12)
            } else {
                entry.name.clone()
            };

            let mut cell_content = column![
                container(thumb_element)
                    .width(Length::Fixed(theme::GRID_THUMB_SIZE))
                    .height(Length::Fixed(theme::GRID_THUMB_SIZE))
                    .center_x(Length::Fixed(theme::GRID_THUMB_SIZE))
                    .center_y(Length::Fixed(theme::GRID_THUMB_SIZE)),
                text(display_name).size(11).center(),
            ]
            .spacing(2)
            .align_x(iced::Alignment::Center);

            if pane.filter_model.scope == iris_search::filter::Scope::Recursive {
                if let Some(relative_parent) = relative_parent_label(&pane.current_path, entry) {
                    cell_content = cell_content.push(
                        text(truncate_with_ellipsis(&relative_parent, 18))
                            .size(9)
                            .color(muted)
                            .center(),
                    );
                }
            }

            if let Some(size) = entry.size {
                cell_content =
                    cell_content.push(text(format_size(size)).size(9).color(muted).center());
            }

            let file_btn = button(
                container(cell_content)
                    .width(Length::Fixed(theme::GRID_CELL_WIDTH))
                    .height(Length::Fixed(theme::GRID_CELL_HEIGHT))
                    .center_x(Length::Fixed(theme::GRID_CELL_WIDTH)),
            )
            .padding(4)
            .style(style);
            let file_btn = if entry.is_inaccessible {
                file_btn
            } else {
                file_btn.on_press(Message::Pane(pane_idx, PaneMessage::SelectItem(index)))
            };
            let cell = if entry.is_inaccessible {
                mouse_area(file_btn).on_press(Message::SetActivePane(pane_idx))
            } else {
                mouse_area(file_btn).on_press(Message::DragStart {
                    pane: pane_idx,
                    index,
                })
            }
            .on_right_press(Message::OpenItemContextMenu {
                pane: pane_idx,
                index,
            });

            row_widget = row_widget.push(cell);
        }
        rows.push((
            0x2000_0000_0000_0000 | row_index as u64,
            file_section_top + row_index as f32 * GRID_FILE_ROW_HEIGHT,
            container(row_widget)
                .height(Length::Fixed(GRID_FILE_ROW_HEIGHT))
                .width(Length::Fill)
                .into(),
        ));
    }

    rows
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct FixedRowWindow {
    start: usize,
    end: usize,
    top_spacer: f32,
    bottom_spacer: f32,
}

fn fixed_row_window(
    scroll_y: f32,
    viewport_height: f32,
    fallback_window_height: f32,
    total_rows: usize,
    row_height: f32,
    section_top: f32,
    overscan_rows: usize,
) -> FixedRowWindow {
    if total_rows == 0 {
        return FixedRowWindow {
            start: 0,
            end: 0,
            top_spacer: 0.0,
            bottom_spacer: 0.0,
        };
    }

    let viewport_height = if viewport_height > 1.0 {
        viewport_height
    } else {
        (fallback_window_height - 220.0).max(row_height)
    };
    let section_height = total_rows as f32 * row_height;
    let overscan_px = overscan_rows as f32 * row_height;
    let viewport_start = scroll_y.max(0.0);
    let viewport_end = viewport_start + viewport_height;

    if viewport_end + overscan_px < section_top {
        return FixedRowWindow {
            start: 0,
            end: 0,
            top_spacer: 0.0,
            bottom_spacer: section_height,
        };
    }
    if viewport_start > section_top + section_height + overscan_px {
        return FixedRowWindow {
            start: 0,
            end: 0,
            top_spacer: section_height,
            bottom_spacer: 0.0,
        };
    }

    let local_scroll = (viewport_start - section_top).max(0.0);
    let first_visible = (local_scroll / row_height).floor().max(0.0) as usize;
    let visible_rows = (viewport_height / row_height).ceil().max(1.0) as usize + 1;
    let start = first_visible
        .saturating_sub(overscan_rows)
        .min(total_rows.saturating_sub(1));
    let end = (first_visible + visible_rows + overscan_rows).min(total_rows);

    FixedRowWindow {
        start,
        end: end.max(start + 1).min(total_rows),
        top_spacer: start as f32 * row_height,
        bottom_spacer: total_rows.saturating_sub(end) as f32 * row_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::local_entry;

    #[test]
    fn relative_parent_label_uses_search_root() {
        let nested = local_entry("/tmp/root/a/b/photo.jpg", false);
        assert_eq!(
            relative_parent_label("/tmp/root", &nested).as_deref(),
            Some("a/b")
        );

        let direct = local_entry("/tmp/root/photo.jpg", false);
        assert_eq!(relative_parent_label("/tmp/root", &direct), None);
    }

    #[test]
    fn relative_parent_label_falls_back_for_external_result() {
        let external = local_entry("/Volumes/archive/photo.jpg", false);
        assert_eq!(
            relative_parent_label("/tmp/root", &external).as_deref(),
            Some("/Volumes/archive")
        );
    }

    #[test]
    fn list_window_keeps_only_visible_rows_with_overscan() {
        let mut pane = PaneState {
            viewport_height: 340.0,
            scroll_y: LIST_HEADER_HEIGHT + LIST_ROW_HEIGHT * 100.0,
            ..PaneState::default()
        };
        pane.visible_list_row_offsets = (0..=1_000)
            .map(|position| position as f32 * LIST_ROW_HEIGHT)
            .collect();

        let window = list_window(&pane, 900.0, 1_000);

        assert_eq!(window.start, 88);
        assert_eq!(window.end, 122);
        assert_eq!(window.top_spacer, 88.0 * LIST_ROW_HEIGHT);
        assert_eq!(window.bottom_spacer, (1_000 - 122) as f32 * LIST_ROW_HEIGHT);

        pane.scroll_y = 0.0;
        let top = list_window(&pane, 900.0, 1_000);
        assert_eq!(top.start, 0);
        assert!(top.end > 0);
        assert_eq!(top.top_spacer, 0.0);
    }

    #[test]
    fn fixed_row_window_preserves_offscreen_section_height() {
        let before_viewport = fixed_row_window(0.0, 300.0, 900.0, 100, 50.0, 1_000.0, 2);
        assert_eq!(before_viewport.start, 0);
        assert_eq!(before_viewport.end, 0);
        assert_eq!(before_viewport.top_spacer, 0.0);
        assert_eq!(before_viewport.bottom_spacer, 5_000.0);

        let after_viewport = fixed_row_window(8_000.0, 300.0, 900.0, 100, 50.0, 1_000.0, 2);
        assert_eq!(after_viewport.start, 0);
        assert_eq!(after_viewport.end, 0);
        assert_eq!(after_viewport.top_spacer, 5_000.0);
        assert_eq!(after_viewport.bottom_spacer, 0.0);
    }
}
