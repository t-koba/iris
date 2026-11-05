use super::pane_list::{view_pane_grid, view_pane_list};
use super::tab_bar::view_tab_bar;
use super::toolbar::{toolbar_icon_with_tip, toolbar_wrap};
use crate::format::{format_size, truncate_with_ellipsis};
use crate::i18n::{tr, tr_with};
use crate::input::dnd::DndTarget;
use crate::messages::{Message, NavigationMessage, PaneMessage, SidebarMessage};
use crate::runtime::app_runtime;
use crate::state::{AppState, PaneLoadState, PaneState, SortKey, SortOrder, ViewMode};
use crate::update::helpers::{is_compact_layout, pane_available_width};
use crate::views::filter::date_range::{two_month_grids, DateRangeSelection, MonthGrid};
use crate::views::filter::map_panel::{
    cluster_points, fit_bounds as fit_gps_points, tile_bounds, tile_coord, visible_tiles,
    GpsBounds as MapGpsBounds, LatLon, MapViewport, TileCoord,
};
use chrono::{Datelike, NaiveDate};
use iced::advanced::image;
use iced::widget::{
    button, canvas, column, container, mouse_area, pick_list, row, scrollable, text, text_input,
    Column,
};
use iced::{mouse, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, Vector};
use iris_search::filter::{FilterModel, GpsFilter, Scope};
use iris_search::segment::IndexedKind;

fn view_pane_breadcrumb_bar(state: &AppState, pane_idx: usize) -> Element<'_, Message> {
    use crate::views::theme;
    use iced::widget::tooltip;

    let pane = state.pane(pane_idx);
    let muted = theme::muted_color(&crate::app::theme(state));
    let display_path = app_runtime().format_display_path(&pane.current_path);

    let mut items = iced::widget::Row::new()
        .spacing(0)
        .align_y(iced::Alignment::Center);
    let last_index = pane.breadcrumb_segments.len().saturating_sub(1);

    for (index, segment) in pane.breadcrumb_segments.iter().enumerate() {
        if index > 0 {
            items = items.push(theme::icon("\u{E5CC}").size(14).color(muted));
        }
        let style = if index == last_index {
            button::primary
        } else {
            button::text
        };
        let message = Message::Pane(pane_idx, PaneMessage::NavigateToBreadcrumb(index));
        items = items.push(
            button(text(truncate_with_ellipsis(&segment.name, 28)).size(12))
                .on_press(message)
                .padding([2, 4])
                .style(style),
        );
    }

    let favorite_group =
        state
            .sidebar_groups
            .iter()
            .enumerate()
            .find_map(|(group_index, group)| {
                if group.kind == crate::state::SidebarGroupKind::Favorites {
                    group
                        .items
                        .iter()
                        .enumerate()
                        .find(|(_, item)| item.path == pane.current_path)
                        .map(|(item_index, _)| (group_index, item_index))
                } else {
                    None
                }
            });
    let favorite_button = if let Some((group, item)) = favorite_group {
        button(theme::icon("\u{E599}").size(14))
            .on_press(SidebarMessage::RemoveItem { group, item }.into())
            .padding([2, 4])
            .style(button::text)
    } else {
        button(theme::icon("\u{E598}").size(14))
            .on_press(SidebarMessage::AddCurrentPathToFavorites.into())
            .padding([2, 4])
            .style(button::text)
    };

    let copy_path_button = button(theme::icon("\u{E14D}").size(14))
        .on_press(NavigationMessage::CopyTextToClipboard(display_path).into())
        .padding([2, 4])
        .style(button::text);
    let edit_path_button = tooltip(
        button(theme::icon("\u{E2C8}").size(14))
            .on_press(NavigationMessage::EditPath { pane: pane_idx }.into())
            .padding([2, 4])
            .style(button::text),
        container(text(tr("pane-edit-path-tip")).size(11))
            .padding([4, 8])
            .style(theme::tooltip_style),
        tooltip::Position::Bottom,
    )
    .gap(2)
    .snap_within_viewport(true);
    let action_buttons = iced::widget::Row::new()
        .spacing(1)
        .align_y(iced::Alignment::Center)
        .push(edit_path_button)
        .push(copy_path_button)
        .push(favorite_button);

    let mut bar_row = iced::widget::Row::new()
        .spacing(4)
        .align_y(iced::Alignment::Center);
    if state.dual_pane_enabled {
        let label = if pane_idx == 0 {
            "\u{F716}"
        } else {
            "\u{F704}"
        };
        let indicator_style = if state.active_pane == pane_idx {
            button::primary
        } else {
            button::text
        };
        bar_row = bar_row.push(
            button(theme::icon(label).size(10))
                .on_press(Message::SetActivePane(pane_idx))
                .padding([2, 4])
                .style(indicator_style),
        );
    }
    let path_area: Element<'_, Message> = if pane.path_editing {
        let input = text_input(&tr("path-bar-placeholder"), &pane.path_input)
            .id(crate::app::ids::path_bar_input_id(pane_idx))
            .on_input(move |value| Message::Pane(pane_idx, PaneMessage::PathInputChanged(value)))
            .on_submit(Message::Pane(pane_idx, PaneMessage::CommitPathEdit))
            .size(12)
            .padding([3, 8])
            .width(Length::Fill);
        let mut editor = iced::widget::Column::new().spacing(2).push(input);
        if !pane.path_suggestions.is_empty() {
            let mut suggestions = iced::widget::Column::new().spacing(1);
            for (index, suggestion) in pane.path_suggestions.iter().enumerate() {
                let style = if pane.path_suggestion_selected == Some(index) {
                    button::primary
                } else {
                    button::text
                };
                suggestions = suggestions.push(
                    button(text(truncate_with_ellipsis(&suggestion.label, 88)).size(11))
                        .on_press(Message::Pane(
                            pane_idx,
                            PaneMessage::CommitPathSuggestion(index),
                        ))
                        .padding([2, 8])
                        .width(Length::Fill)
                        .style(style),
                );
            }
            editor = editor.push(
                container(suggestions)
                    .width(Length::Fill)
                    .padding([2, 0])
                    .style(theme::breadcrumb_style),
            );
        }
        container(editor).width(Length::Fill).into()
    } else {
        container(
            scrollable(items).direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::default().width(0).scroller_width(0),
            )),
        )
        .width(Length::Fill)
        .center_y(Length::Shrink)
        .into()
    };
    bar_row = bar_row.push(path_area);
    bar_row = bar_row.push(action_buttons);

    container(bar_row)
        .style(theme::breadcrumb_style)
        .padding([2, crate::views::theme::ZONE_PADDING as u16])
        .width(Length::Fill)
        .height(Length::Fixed(theme::BREADCRUMB_BAR_HEIGHT))
        .into()
}

fn view_filter_bar(state: &AppState, pane_idx: usize) -> Element<'_, Message> {
    let pane = state.pane(pane_idx);
    let muted = crate::views::theme::muted_color(&crate::app::theme(state));
    let input = text_input(&tr("pane-filter-placeholder"), &pane.filter_query)
        .id(crate::app::ids::filter_input_id(pane_idx))
        .on_input(move |value| Message::Pane(pane_idx, PaneMessage::SetFilter(value)))
        .size(12)
        .padding([3, 8])
        .width(Length::Fill);

    let match_count = pane.visible_indices().len();
    let total_count = pane.entries.len();
    let count_text = if pane.filter_recursive_running {
        tr_with(
            "pane-filter-recursive-progress",
            &[
                ("matches", pane.filter_recursive_matched.into()),
                ("scanned", pane.filter_recursive_scanned.into()),
            ],
        )
    } else if pane.is_filter_active() && pane.load_state.is_loading() {
        tr_with(
            "pane-filter-match-count-loading",
            &[
                ("matches", match_count.into()),
                ("total", total_count.into()),
            ],
        )
    } else if pane.is_filter_active() {
        tr_with(
            "pane-filter-match-count",
            &[
                ("matches", match_count.into()),
                ("total", total_count.into()),
            ],
        )
    } else if pane.load_state.is_loading() {
        tr_with(
            "pane-filter-total-count-loading",
            &[("total", total_count.into())],
        )
    } else {
        tr_with("pane-filter-total-count", &[("total", total_count.into())])
    };

    let active_chips = filter_chips(&pane.filter_model);
    let facet_chips = filter_facet_chips(pane_idx, pane);
    let has_filter_details = !active_chips.is_empty()
        || !facet_chips.is_empty()
        || !pane.filter_warnings.is_empty()
        || pane.filter_recursive_running;
    let primary_row = row![input, text(count_text).size(11)]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .width(Length::Fill);
    let mut actions = iced::widget::Row::new()
        .spacing(4)
        .align_y(iced::Alignment::Center);
    if pane.filter_recursive_running {
        actions = actions.push(
            button(text(tr("pane-filter-cancel")).size(11))
                .padding([2, 8])
                .on_press(Message::Pane(pane_idx, PaneMessage::CancelRecursiveFilter)),
        );
    }
    actions = actions.push(
        button(text(tr("filter-date-range-toggle")).size(11))
            .padding([2, 8])
            .style(button::text)
            .on_press(Message::Pane(pane_idx, PaneMessage::ToggleDateRangePicker)),
    );
    actions = actions.push(
        button(text(tr("filter-gps-bounds-toggle")).size(11))
            .padding([2, 8])
            .style(button::text)
            .on_press(Message::Pane(pane_idx, PaneMessage::ToggleGpsBoundsPicker)),
    );
    let top: Element<'_, Message> = if pane_available_width(state, pane_idx) < 420.0 {
        column![primary_row, actions].spacing(2).into()
    } else {
        row![primary_row, actions]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .into()
    };
    let mut filter_content = Column::new().push(top).spacing(2);
    if !active_chips.is_empty() || !facet_chips.is_empty() || !pane.filter_warnings.is_empty() {
        let mut chip_row = iced::widget::Row::new()
            .spacing(6)
            .align_y(iced::Alignment::Center);
        for chip in active_chips {
            chip_row = chip_row.push(text(chip).size(10).color(muted));
        }
        for chip in facet_chips {
            chip_row = chip_row.push(
                button(text(chip.label).size(10))
                    .padding([1, 6])
                    .style(button::text)
                    .on_press(Message::Pane(
                        pane_idx,
                        PaneMessage::AppendFilterToken(chip.token),
                    )),
            );
        }
        if !pane.filter_warnings.is_empty() {
            chip_row = chip_row.push(
                text(tr_with(
                    "pane-filter-warning-count",
                    &[("count", pane.filter_warnings.len().into())],
                ))
                .size(10)
                .color(iced::Color::from_rgb8(190, 90, 45)),
            );
        }
        filter_content = filter_content.push(scrollable(chip_row).direction(
            scrollable::Direction::Horizontal(
                scrollable::Scrollbar::default().width(0).scroller_width(0),
            ),
        ));
    }
    if pane.filter_date_picker_open {
        filter_content = filter_content.push(date_range_picker_row(pane_idx, pane));
    }
    if pane.filter_gps_picker_open {
        filter_content = filter_content.push(gps_bounds_picker_row(pane_idx, pane));
    }

    let height = if pane.filter_date_picker_open {
        if pane.filter_gps_picker_open {
            398.0
        } else {
            234.0
        }
    } else if pane.filter_gps_picker_open {
        218.0
    } else if pane.is_filter_active() && has_filter_details {
        52.0
    } else {
        32.0
    };

    container(filter_content)
        .padding([2, crate::views::theme::ZONE_PADDING as u16])
        .height(Length::Fixed(height))
        .width(Length::Fill)
        .style(crate::views::theme::sunken_panel_style)
        .into()
}

fn gps_bounds_picker_row(pane_idx: usize, pane: &PaneState) -> Element<'_, Message> {
    let preview = parse_gps_bounds_inputs(pane);
    let points = gps_canvas_points(pane);
    let tiles = gps_canvas_tiles(pane);
    let selected_bounds = preview.as_ref().map(|(bounds, _)| *bounds);
    let viewport_bounds = selected_bounds
        .or_else(|| fit_gps_points(&points))
        .unwrap_or_else(world_gps_bounds);
    let input_row = row![
        text(tr("filter-gps-bounds-label")).size(10),
        text_input("min lat", &pane.filter_gps_min_lat_input)
            .size(10)
            .padding([2, 6])
            .width(Length::Fixed(76.0))
            .on_input(move |value| Message::Pane(pane_idx, PaneMessage::SetGpsMinLat(value))),
        text_input("max lat", &pane.filter_gps_max_lat_input)
            .size(10)
            .padding([2, 6])
            .width(Length::Fixed(76.0))
            .on_input(move |value| Message::Pane(pane_idx, PaneMessage::SetGpsMaxLat(value))),
        text_input("min lon", &pane.filter_gps_min_lon_input)
            .size(10)
            .padding([2, 6])
            .width(Length::Fixed(76.0))
            .on_input(move |value| Message::Pane(pane_idx, PaneMessage::SetGpsMinLon(value))),
        text_input("max lon", &pane.filter_gps_max_lon_input)
            .size(10)
            .padding([2, 6])
            .width(Length::Fixed(76.0))
            .on_input(move |value| Message::Pane(pane_idx, PaneMessage::SetGpsMaxLon(value))),
        button(text(tr("common-apply")).size(10))
            .padding([1, 6])
            .on_press(Message::Pane(pane_idx, PaneMessage::ApplyGpsBounds)),
        button(text(tr("common-clear")).size(10))
            .padding([1, 6])
            .style(button::text)
            .on_press(Message::Pane(pane_idx, PaneMessage::ClearGpsBounds)),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let mut content = column![input_row].spacing(2);
    if let Some((_, summary)) = &preview {
        content = content.push(
            text(summary.clone())
                .size(10)
                .style(crate::views::theme::muted_text_style),
        );
    }
    content = content.push(
        canvas::Canvas::new(GpsBoundsCanvas {
            pane_idx,
            viewport_bounds,
            selected_bounds,
            points,
            tiles,
        })
        .width(Length::Fill)
        .height(Length::Fixed(140.0)),
    );
    content.into()
}

fn date_range_picker_row(pane_idx: usize, pane: &PaneState) -> Element<'_, Message> {
    let anchor = pane
        .filter_date_anchor
        .or_else(|| parse_date_input(&pane.filter_date_start_input))
        .unwrap_or_else(|| chrono::Local::now().date_naive());
    let selection = DateRangeSelection {
        start: parse_date_input(&pane.filter_date_start_input),
        end: parse_date_input(&pane.filter_date_end_input),
    }
    .normalized();
    let marked_dates = marked_entry_dates(pane);
    let [first, second] = two_month_grids(anchor, selection, &marked_dates);
    let label = match (selection.start, selection.end) {
        (Some(start), Some(end)) => {
            format!("{} - {}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"))
        }
        (Some(start), None) => start.format("%Y-%m-%d").to_string(),
        _ => tr("filter-date-range-empty"),
    };

    let header = row![
        button(text("<").size(10))
            .padding([1, 6])
            .style(button::text)
            .on_press(Message::Pane(pane_idx, PaneMessage::PreviousDateRangeMonth)),
        text(label).size(10).width(Length::Fill),
        button(text(tr("filter-date-preset-today")).size(10))
            .padding([1, 6])
            .style(button::text)
            .on_press(Message::Pane(pane_idx, PaneMessage::PresetDateToday)),
        button(text(tr("filter-date-preset-month")).size(10))
            .padding([1, 6])
            .style(button::text)
            .on_press(Message::Pane(pane_idx, PaneMessage::PresetDateMonth)),
        button(text(tr("filter-date-preset-year")).size(10))
            .padding([1, 6])
            .style(button::text)
            .on_press(Message::Pane(pane_idx, PaneMessage::PresetDateYear)),
        button(text(tr("common-apply")).size(10))
            .padding([1, 6])
            .on_press(Message::Pane(pane_idx, PaneMessage::ApplyDateRange)),
        button(text(tr("common-clear")).size(10))
            .padding([1, 6])
            .style(button::text)
            .on_press(Message::Pane(pane_idx, PaneMessage::ClearDateRange)),
        button(text(">").size(10))
            .padding([1, 6])
            .style(button::text)
            .on_press(Message::Pane(pane_idx, PaneMessage::NextDateRangeMonth)),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center);

    column![
        header,
        row![
            month_grid_view(pane_idx, first),
            month_grid_view(pane_idx, second),
        ]
        .spacing(10)
    ]
    .spacing(4)
    .into()
}

fn month_grid_view(pane_idx: usize, grid: MonthGrid) -> Element<'static, Message> {
    let mut content = column![text(format!("{}-{:02}", grid.year, grid.month)).size(10)]
        .spacing(2)
        .width(Length::Shrink);
    let mut weekdays = iced::widget::Row::new().spacing(2);
    for label in ["M", "T", "W", "T", "F", "S", "S"] {
        weekdays = weekdays.push(
            container(
                text(label)
                    .size(9)
                    .style(crate::views::theme::muted_text_style),
            )
            .width(Length::Fixed(24.0))
            .center_x(Length::Fixed(24.0)),
        );
    }
    content = content.push(weekdays);

    for week in grid.cells.chunks(7) {
        let mut row = iced::widget::Row::new().spacing(2);
        for cell in week {
            let label = if cell.has_data {
                format!("{}*", cell.date.day())
            } else {
                cell.date.day().to_string()
            };
            let mut day = button(text(label).size(9))
                .padding([1, 0])
                .width(Length::Fixed(24.0))
                .on_press(Message::Pane(
                    pane_idx,
                    PaneMessage::SelectDateRangeDay(cell.date.format("%Y-%m-%d").to_string()),
                ));
            day = if cell.in_range {
                day.style(button::primary)
            } else {
                day.style(button::text)
            };
            let element: Element<'_, Message> = if cell.in_month {
                day.into()
            } else {
                container(day)
                    .style(|theme| container::Style {
                        text_color: Some(crate::views::theme::muted_color(theme)),
                        ..container::Style::default()
                    })
                    .into()
            };
            row = row.push(element);
        }
        content = content.push(row);
    }

    content.into()
}

fn marked_entry_dates(pane: &PaneState) -> std::collections::BTreeSet<NaiveDate> {
    pane.entries
        .iter()
        .filter_map(|entry| entry.mtime)
        .map(|mtime| {
            let datetime: chrono::DateTime<chrono::Local> = mtime.into();
            datetime.date_naive()
        })
        .collect()
}

fn parse_date_input(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok()
}

fn parse_gps_bounds_inputs(pane: &PaneState) -> Option<(MapGpsBounds, String)> {
    let min_lat = pane.filter_gps_min_lat_input.trim().parse::<f64>().ok()?;
    let max_lat = pane.filter_gps_max_lat_input.trim().parse::<f64>().ok()?;
    let min_lon = pane.filter_gps_min_lon_input.trim().parse::<f64>().ok()?;
    let max_lon = pane.filter_gps_max_lon_input.trim().parse::<f64>().ok()?;
    if !(-90.0..=90.0).contains(&min_lat)
        || !(-90.0..=90.0).contains(&max_lat)
        || !(-180.0..=180.0).contains(&min_lon)
        || !(-180.0..=180.0).contains(&max_lon)
        || min_lat > max_lat
    {
        return None;
    }

    let bounds = MapGpsBounds {
        min_lat,
        max_lat,
        min_lon,
        max_lon,
    };
    let center = LatLon {
        lat: (min_lat + max_lat) / 2.0,
        lon: (min_lon + max_lon) / 2.0,
    };
    let tile = tile_coord(center, 8);
    let summary = tr_with(
        "filter-gps-bounds-summary",
        &[
            (
                "center",
                format!("{:.5}, {:.5}", center.lat, center.lon).into(),
            ),
            ("tile", format!("{}/{}/{}", tile.z, tile.x, tile.y).into()),
        ],
    );
    Some((bounds, summary))
}

#[derive(Clone)]
struct GpsBoundsCanvas {
    pane_idx: usize,
    viewport_bounds: MapGpsBounds,
    selected_bounds: Option<MapGpsBounds>,
    points: Vec<LatLon>,
    tiles: Vec<MapTileImage>,
}

#[derive(Clone)]
struct MapTileImage {
    tile: TileCoord,
    handle: image::Handle,
}

#[derive(Default)]
struct GpsBoundsCanvasState {
    viewport_bounds: Option<MapGpsBounds>,
    viewport: Option<MapViewport>,
    select_start: Option<Point>,
    select_current: Option<Point>,
    pan_last: Option<Point>,
}

impl canvas::Program<Message> for GpsBoundsCanvas {
    type State = GpsBoundsCanvasState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<iced::widget::Action<Message>> {
        ensure_gps_viewport(state, self.viewport_bounds, bounds.size());
        let visible_tiles_message = state.viewport.map(|viewport| {
            Message::Pane(
                self.pane_idx,
                PaneMessage::GpsMapVisibleTiles(visible_tiles(
                    viewport,
                    bounds.width.max(1.0),
                    bounds.height.max(1.0),
                )),
            )
        });
        let position = cursor.position_in(bounds)?;

        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                state.select_start = Some(position);
                state.select_current = Some(position);
                Some(canvas_capture(visible_tiles_message))
            }
            canvas::Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Middle | mouse::Button::Right,
            )) => {
                state.pan_last = Some(position);
                Some(canvas_capture(visible_tiles_message))
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if state.select_start.is_some() {
                    state.select_current = Some(position);
                    return Some(canvas_capture(None));
                }
                if let (Some(last), Some(viewport)) = (state.pan_last, state.viewport.as_mut()) {
                    let delta: Vector = position - last;
                    viewport.pan_by_pixels(delta.x, delta.y);
                    state.pan_last = Some(position);
                    let message = Some(Message::Pane(
                        self.pane_idx,
                        PaneMessage::GpsMapVisibleTiles(visible_tiles(
                            *viewport,
                            bounds.width.max(1.0),
                            bounds.height.max(1.0),
                        )),
                    ));
                    return Some(canvas_capture(message));
                }
                None
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let message = if let (Some(start), Some(current), Some(viewport)) =
                    (state.select_start, state.select_current, state.viewport)
                {
                    let width = bounds.width.max(1.0);
                    let height = bounds.height.max(1.0);
                    let delta = current - start;
                    if delta.x.abs() >= 4.0 && delta.y.abs() >= 4.0 {
                        let selected = viewport.bounds_from_screen_rect(
                            (start.x, start.y),
                            (current.x, current.y),
                            width,
                            height,
                        );
                        Some(Message::Pane(
                            self.pane_idx,
                            PaneMessage::ApplyGpsBoundsFromMap {
                                min_lat: selected.min_lat as f32,
                                max_lat: selected.max_lat as f32,
                                min_lon: selected.min_lon as f32,
                                max_lon: selected.max_lon as f32,
                            },
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                };
                state.select_start = None;
                state.select_current = None;
                Some(canvas_capture(message))
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(
                mouse::Button::Middle | mouse::Button::Right,
            )) => {
                state.pan_last = None;
                Some(canvas_capture(None))
            }
            canvas::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                if let Some(viewport) = state.viewport.as_mut() {
                    let steps = match delta {
                        mouse::ScrollDelta::Lines { y, .. } => y.signum() as i8,
                        mouse::ScrollDelta::Pixels { y, .. } => (y / 64.0).signum() as i8,
                    };
                    if steps != 0 {
                        viewport.zoom_by_steps(steps);
                        let message = Some(Message::Pane(
                            self.pane_idx,
                            PaneMessage::GpsMapVisibleTiles(visible_tiles(
                                *viewport,
                                bounds.width.max(1.0),
                                bounds.height.max(1.0),
                            )),
                        ));
                        return Some(canvas_capture(message));
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let area = Rectangle {
            x: 0.0,
            y: 0.0,
            width: frame.width(),
            height: frame.height(),
        };
        frame.fill_rectangle(Point::ORIGIN, frame.size(), Color::from_rgb8(22, 26, 30));
        let viewport = state.viewport.unwrap_or_else(|| {
            MapViewport::fit_bounds(self.viewport_bounds, frame.width(), frame.height(), 18.0)
        });
        draw_map_tiles(&mut frame, area, viewport, &self.tiles);
        draw_gps_grid(&mut frame, area, viewport);
        draw_gps_points(&mut frame, area, viewport, &self.points);
        if let Some(bounds) = self.selected_bounds {
            draw_gps_bounds(&mut frame, area, viewport, bounds);
        }
        if let (Some(start), Some(current)) = (state.select_start, state.select_current) {
            draw_gps_selection(&mut frame, start, current);
        }
        frame.fill_text(canvas::Text {
            content: "© OpenStreetMap contributors".to_string(),
            position: Point::new(8.0, frame.height() - 16.0),
            color: Color::from_rgba8(220, 224, 228, 0.75),
            size: iced::Pixels(10.0),
            ..canvas::Text::default()
        });
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.pan_last.is_some() {
            mouse::Interaction::Grabbing
        } else if state.select_start.is_some() || cursor.is_over(bounds) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::None
        }
    }
}

fn canvas_capture(message: Option<Message>) -> iced::widget::Action<Message> {
    match message {
        Some(message) => iced::widget::Action::publish(message).and_capture(),
        None => iced::widget::Action::capture(),
    }
}

fn ensure_gps_viewport(state: &mut GpsBoundsCanvasState, bounds: MapGpsBounds, size: Size) {
    if state.viewport_bounds != Some(bounds) {
        state.viewport_bounds = Some(bounds);
        state.viewport = None;
        state.select_start = None;
        state.select_current = None;
        state.pan_last = None;
    }
    state
        .viewport
        .get_or_insert_with(|| MapViewport::fit_bounds(bounds, size.width, size.height, 18.0));
}

fn gps_canvas_points(pane: &PaneState) -> Vec<LatLon> {
    pane.filter_gps_points
        .iter()
        .map(|(lat, lon)| LatLon {
            lat: f64::from(*lat),
            lon: f64::from(*lon),
        })
        .collect()
}

fn gps_canvas_tiles(pane: &PaneState) -> Vec<MapTileImage> {
    pane.filter_map_tiles
        .iter()
        .map(|(tile, handle)| MapTileImage {
            tile: *tile,
            handle: handle.clone(),
        })
        .collect()
}

fn world_gps_bounds() -> MapGpsBounds {
    MapGpsBounds {
        min_lat: -85.0,
        max_lat: 85.0,
        min_lon: -180.0,
        max_lon: 180.0,
    }
}

fn draw_gps_grid(frame: &mut canvas::Frame, area: Rectangle, viewport: MapViewport) {
    let stroke = canvas::Stroke::default()
        .with_color(Color::from_rgba8(160, 170, 180, 0.25))
        .with_width(1.0);
    for rect in visible_tiles(viewport, area.width, area.height)
        .into_iter()
        .filter_map(|tile| tile_screen_rect(tile, area, viewport))
    {
        frame.stroke(
            &canvas::Path::rectangle(rect.position(), rect.size()),
            stroke,
        );
    }
}

fn draw_map_tiles(
    frame: &mut canvas::Frame,
    area: Rectangle,
    viewport: MapViewport,
    tiles: &[MapTileImage],
) {
    for tile in tiles {
        if tile.tile.z != viewport.zoom {
            continue;
        }
        let Some(rect) = tile_screen_rect(tile.tile, area, viewport) else {
            continue;
        };
        frame.draw_image(rect, image::Image::new(tile.handle.clone()));
    }
}

fn tile_screen_rect(tile: TileCoord, area: Rectangle, viewport: MapViewport) -> Option<Rectangle> {
    let bounds = tile_bounds(tile);
    let (left, top) = viewport.world_to_screen(
        LatLon {
            lat: bounds.max_lat,
            lon: bounds.min_lon,
        },
        area.width,
        area.height,
    );
    let (right, bottom) = viewport.world_to_screen(
        LatLon {
            lat: bounds.min_lat,
            lon: bounds.max_lon,
        },
        area.width,
        area.height,
    );
    let rect = Rectangle {
        x: left.min(right),
        y: top.min(bottom),
        width: (right - left).abs(),
        height: (bottom - top).abs(),
    };
    (rect.width > 0.5 && rect.height > 0.5).then_some(rect)
}

fn draw_gps_bounds(
    frame: &mut canvas::Frame,
    area: Rectangle,
    viewport: MapViewport,
    bounds: MapGpsBounds,
) {
    let (left, top) = viewport.world_to_screen(
        LatLon {
            lat: bounds.max_lat,
            lon: bounds.min_lon,
        },
        area.width,
        area.height,
    );
    let (right, bottom) = viewport.world_to_screen(
        LatLon {
            lat: bounds.min_lat,
            lon: bounds.max_lon,
        },
        area.width,
        area.height,
    );
    let rect_pos = Point::new(left.min(right), top.min(bottom));
    let rect_size = Size::new((right - left).abs().max(3.0), (bottom - top).abs().max(3.0));
    frame.fill_rectangle(rect_pos, rect_size, Color::from_rgba8(82, 139, 255, 0.22));
    frame.stroke_rectangle(
        rect_pos,
        rect_size,
        canvas::Stroke::default()
            .with_color(Color::from_rgb8(115, 166, 255))
            .with_width(1.5),
    );
    let center = Point::new(
        rect_pos.x + rect_size.width / 2.0,
        rect_pos.y + rect_size.height / 2.0,
    );
    frame.fill(
        &canvas::Path::circle(center, 3.5),
        Color::from_rgb8(240, 245, 255),
    );
}

fn draw_gps_points(
    frame: &mut canvas::Frame,
    area: Rectangle,
    viewport: MapViewport,
    points: &[LatLon],
) {
    let clusters = cluster_points(points, viewport.zoom, 44.0);
    for cluster in clusters {
        let (x, y) = viewport.world_to_screen(cluster.center, area.width, area.height);
        let center = Point::new(x, y);
        if !area.contains(center) {
            continue;
        }
        let radius = if cluster.count > 1 { 6.0 } else { 3.5 };
        frame.fill(
            &canvas::Path::circle(center, radius),
            Color::from_rgba8(255, 214, 102, 0.9),
        );
        frame.stroke(
            &canvas::Path::circle(center, radius),
            canvas::Stroke::default()
                .with_color(Color::from_rgb8(45, 34, 12))
                .with_width(1.0),
        );
    }
}

fn draw_gps_selection(frame: &mut canvas::Frame, start: Point, current: Point) {
    let rect_pos = Point::new(start.x.min(current.x), start.y.min(current.y));
    let rect_size = Size::new((current.x - start.x).abs(), (current.y - start.y).abs());
    frame.fill_rectangle(rect_pos, rect_size, Color::from_rgba8(255, 255, 255, 0.12));
    frame.stroke_rectangle(
        rect_pos,
        rect_size,
        canvas::Stroke::default()
            .with_color(Color::from_rgb8(245, 247, 250))
            .with_width(1.0),
    );
}

fn filter_chips(model: &FilterModel) -> Vec<String> {
    let mut chips = Vec::new();
    if model.scope == Scope::Recursive {
        chips.push(tr("filter-chip-scope-recursive"));
    }
    if model.content {
        chips.push(tr("filter-chip-content"));
    }
    for kind in &model.kinds {
        chips.push(tr_with(
            "filter-chip-kind",
            &[("kind", filter_kind_label(*kind).into())],
        ));
    }
    if let Some(size) = &model.size {
        match (size.min, size.max) {
            (Some(min), Some(max)) if min == max => {
                chips.push(tr_with(
                    "filter-chip-size-exact",
                    &[("size", format_size(min).into())],
                ));
            }
            (Some(min), Some(max)) => {
                chips.push(tr_with(
                    "filter-chip-size-range",
                    &[
                        ("min", format_size(min).into()),
                        ("max", format_size(max).into()),
                    ],
                ));
            }
            (Some(min), None) => {
                chips.push(tr_with(
                    "filter-chip-size-min",
                    &[("size", format_size(min).into())],
                ));
            }
            (None, Some(max)) => {
                chips.push(tr_with(
                    "filter-chip-size-max",
                    &[("size", format_size(max).into())],
                ));
            }
            (None, None) => {}
        }
    }
    if let Some(date) = &model.date {
        match (date.min, date.max) {
            (Some(min), Some(max)) => chips.push(tr_with(
                "filter-chip-date-range",
                &[
                    ("start", format_filter_date(min).into()),
                    ("end", format_filter_date(max).into()),
                ],
            )),
            (Some(min), None) => chips.push(tr_with(
                "filter-chip-date-after",
                &[("date", format_filter_date(min).into())],
            )),
            (None, Some(max)) => chips.push(tr_with(
                "filter-chip-date-before",
                &[("date", format_filter_date(max).into())],
            )),
            (None, None) => {}
        }
    }
    if let Some(camera) = model.camera.as_deref() {
        chips.push(tr_with("filter-chip-camera", &[("camera", camera.into())]));
    }
    if let Some(lens) = model.lens.as_deref() {
        chips.push(tr_with("filter-chip-lens", &[("lens", lens.into())]));
    }
    match &model.gps {
        GpsFilter::Any => {}
        GpsFilter::Present => chips.push(tr("filter-chip-gps-present")),
        GpsFilter::Absent => chips.push(tr("filter-chip-gps-absent")),
        GpsFilter::Bounds(_) => chips.push(tr("filter-chip-gps-bounds")),
    }
    chips
}

struct FilterFacetChip {
    label: String,
    token: String,
}

fn filter_facet_chips(pane_idx: usize, pane: &PaneState) -> Vec<FilterFacetChip> {
    let mut chips = Vec::new();
    for (&kind, &count) in &pane.facet_counts.kind_counts {
        if count == 0 || pane.filter_model.kinds.contains(&kind) {
            continue;
        }
        let label = tr_with(
            "filter-chip-kind",
            &[("kind", filter_kind_label(kind).into())],
        );
        chips.push(FilterFacetChip {
            label: facet_label(&label, count),
            token: format!("kind:{}", filter_kind_token(kind)),
        });
    }

    if pane.filter_model.size.is_none() {
        push_size_facet(
            &mut chips,
            pane.facet_counts.small_files,
            tr_with("filter-chip-size-max", &[("size", format_size(MIB).into())]),
            format!("size:<{MIB}"),
        );
        push_size_facet(
            &mut chips,
            pane.facet_counts.medium_files,
            tr_with(
                "filter-chip-size-range",
                &[
                    ("min", format_size(MIB).into()),
                    ("max", format_size(100 * MIB).into()),
                ],
            ),
            format!("size:{MIB}..{}", 100 * MIB),
        );
        push_size_facet(
            &mut chips,
            pane.facet_counts.large_files,
            tr_with(
                "filter-chip-size-min",
                &[("size", format_size(100 * MIB).into())],
            ),
            format!("size:>{}", 100 * MIB),
        );
    }

    chips
        .into_iter()
        .take(if pane_idx == 0 { 8 } else { 6 })
        .collect()
}

const MIB: u64 = 1024 * 1024;

fn push_size_facet(chips: &mut Vec<FilterFacetChip>, count: usize, label: String, token: String) {
    if count > 0 {
        chips.push(FilterFacetChip {
            label: facet_label(&label, count),
            token,
        });
    }
}

fn facet_label(label: &str, count: usize) -> String {
    tr_with(
        "filter-facet-chip",
        &[("label", label.into()), ("count", count.into())],
    )
}

fn filter_kind_label(kind: IndexedKind) -> &'static str {
    match kind {
        IndexedKind::File => "file",
        IndexedKind::Directory => "folder",
        IndexedKind::Image => "image",
        IndexedKind::Video => "video",
        IndexedKind::Pdf => "pdf",
        IndexedKind::Other => "other",
    }
}

fn filter_kind_token(kind: IndexedKind) -> &'static str {
    match kind {
        IndexedKind::File => "file",
        IndexedKind::Directory => "folder",
        IndexedKind::Image => "image",
        IndexedKind::Video => "video",
        IndexedKind::Pdf => "pdf",
        IndexedKind::Other => "other",
    }
}

fn format_filter_date(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|date| date.date_naive().format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

pub(super) fn view_pane_content(state: &AppState, pane_idx: usize) -> Element<'_, Message> {
    let pane = state.pane(pane_idx);
    let compact = is_compact_layout(state);
    let is_pane_drop_target =
        state.dnd_state.target() == Some(DndTarget::PaneDirectory { pane: pane_idx });

    let breadcrumb = view_pane_breadcrumb_bar(state, pane_idx);
    let tab_bar = view_tab_bar(state, pane_idx);
    let filter_bar = view_filter_bar(state, pane_idx);

    let content: Element<'_, Message> = if let PaneLoadState::Failed { message } = &pane.load_state
    {
        let retry_path = pane.current_path.clone();
        // A folder that was deleted or a volume that vanished would otherwise be a dead end.
        let parent_path = std::path::Path::new(&pane.current_path)
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .filter(|parent| {
                app_runtime().is_virtual_display_path(parent)
                    || !app_runtime().is_virtual_display_path(&pane.current_path)
            });
        let up_button = parent_path.map(|parent| {
            button(
                row![
                    crate::views::theme::icon("\u{E5D8}").size(14),
                    text(tr("toolbar-up")).size(12),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            )
            .padding([7, 14])
            .style(button::secondary)
            .on_press(Message::Pane(pane_idx, PaneMessage::NavigateToPath(parent)))
        });
        container(
            column![
                crate::views::theme::icon("\u{F8B6}")
                    .size(36)
                    .style(|theme| iced::widget::text::Style {
                        color: Some(theme.extended_palette().danger.base.color),
                    }),
                text(tr_with("pane-error", &[("error", message.into())])).size(14),
                text(app_runtime().format_display_path(&pane.current_path))
                    .size(11)
                    .color(crate::views::theme::muted_color(&crate::app::theme(state))),
                button(
                    row![
                        crate::views::theme::icon("\u{E5D5}").size(14),
                        text(tr("pane-retry")).size(12),
                    ]
                    .spacing(6)
                    .align_y(iced::Alignment::Center),
                )
                .padding([7, 14])
                .on_press(Message::Pane(
                    pane_idx,
                    PaneMessage::NavigateToPath(retry_path),
                )),
            ]
            .push(up_button)
            .spacing(12)
            .align_x(iced::Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    } else if pane.entries.is_empty() && pane.load_state.is_loading() {
        pane_state_placeholder("\u{E88B}", tr("pane-loading"))
    } else if pane.entries.is_empty() {
        pane_state_placeholder("\u{E2C7}", tr("pane-empty-directory"))
    } else if pane.is_filter_active() && pane.visible_indices().is_empty() {
        pane_state_placeholder("\u{E8F5}", tr("pane-filter-empty"))
    } else {
        match pane.view_mode {
            ViewMode::List => view_pane_list(state, pane_idx, compact),
            ViewMode::Grid => view_pane_grid(state, pane_idx),
        }
    };

    let content_container = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true)
        .style(move |theme: &iced::Theme| {
            if is_pane_drop_target {
                crate::views::theme::sidebar_drop_target_style(theme)
            } else {
                container::Style::default()
            }
        });
    let pane_drop_area: Element<'_, Message> = mouse_area(content_container)
        .on_enter(Message::DragHover(DndTarget::PaneDirectory {
            pane: pane_idx,
        }))
        .on_move(move |_| Message::DragHover(DndTarget::PaneDirectory { pane: pane_idx }))
        .on_exit(Message::DragLeave(DndTarget::PaneDirectory {
            pane: pane_idx,
        }))
        .into();

    if state.dual_pane_enabled {
        let run_for_pane = |message: Message| Message::ActivatePaneAndRun {
            pane: pane_idx,
            message: Box::new(message),
        };
        let nav_buttons = row![
            toolbar_icon_with_tip(
                "\u{E5C4}",
                tr("toolbar-back"),
                if pane.can_go_back() {
                    Some(run_for_pane(NavigationMessage::Back.into()))
                } else {
                    None
                },
            ),
            toolbar_icon_with_tip(
                "\u{E5C8}",
                tr("toolbar-forward"),
                if pane.can_go_forward() {
                    Some(run_for_pane(NavigationMessage::Forward.into()))
                } else {
                    None
                },
            ),
            toolbar_icon_with_tip(
                "\u{E5D8}",
                tr("toolbar-up"),
                Some(run_for_pane(NavigationMessage::Up.into()))
            ),
            toolbar_icon_with_tip(
                "\u{E5D5}",
                tr("toolbar-refresh"),
                Some(run_for_pane(Message::Refresh))
            ),
        ]
        .spacing(1)
        .align_y(iced::Alignment::Center);

        let has_selection = !pane.selected_indices.is_empty();
        let has_single = pane.selected_indices.len() == 1;
        let single_index = if has_single {
            pane.selected_indices.iter().copied().next()
        } else {
            None
        };
        let can_mutate = state.pending_operation.is_none() && state.active_dialog.is_none();
        let action_buttons = row![
            toolbar_icon_with_tip(
                "\u{E9A2}",
                tr("common-rename"),
                if has_single && can_mutate {
                    single_index.map(|index| run_for_pane(Message::Rename(index)))
                } else if has_selection && can_mutate {
                    Some(run_for_pane(Message::BatchRenameSelected))
                } else {
                    None
                }
            ),
            toolbar_icon_with_tip(
                "\u{E92E}",
                tr("common-delete"),
                if has_selection && can_mutate {
                    Some(run_for_pane(Message::DeleteSelected))
                } else {
                    None
                }
            ),
            toolbar_icon_with_tip(
                "\u{E2CC}",
                tr("toolbar-new-folder"),
                can_mutate.then(|| run_for_pane(Message::CreateFolder))
            ),
        ]
        .spacing(1)
        .align_y(iced::Alignment::Center);

        let sort_picker = pick_list(SortKey::ALL.as_slice(), Some(pane.sort_key), {
            let pane_index = pane_idx;
            move |key| Message::Pane(pane_index, PaneMessage::SetSortKey(key))
        })
        .text_size(11)
        .padding([2, 4]);

        let view_buttons = row![
            sort_picker,
            toolbar_icon_with_tip(
                if pane.sort_order == SortOrder::Ascending {
                    "\u{E5D8}"
                } else {
                    "\u{E5DB}"
                },
                tr("toolbar-sort-order"),
                Some(Message::Pane(pane_idx, PaneMessage::ToggleSortOrder)),
            ),
            toolbar_icon_with_tip(
                match pane.view_mode {
                    ViewMode::Grid => "\u{E8EF}",
                    ViewMode::List => "\u{E9B0}",
                },
                tr("toolbar-view-mode"),
                Some(Message::Pane(
                    pane_idx,
                    PaneMessage::SetViewMode(match pane.view_mode {
                        ViewMode::Grid => ViewMode::List,
                        ViewMode::List => ViewMode::Grid,
                    })
                )),
            ),
            toolbar_icon_with_tip(
                if state.show_hidden_files {
                    "\u{E8F4}"
                } else {
                    "\u{E8F5}"
                },
                tr("toolbar-hidden-files"),
                Some(run_for_pane(Message::ToggleHiddenFiles)),
            ),
        ]
        .spacing(1)
        .align_y(iced::Alignment::Center);

        let (copy_tip, move_tip) = if pane_idx == 0 {
            (tr("pane-copy-right"), tr("pane-move-right"))
        } else {
            (tr("pane-copy-left"), tr("pane-move-left"))
        };
        let cross_buttons = row![
            toolbar_icon_with_tip(
                "\u{E173}",
                copy_tip,
                if has_selection && can_mutate {
                    Some(run_for_pane(Message::CopyToOtherPane))
                } else {
                    None
                }
            ),
            toolbar_icon_with_tip(
                "\u{E1FF}",
                move_tip,
                if has_selection && can_mutate {
                    Some(run_for_pane(Message::MoveToOtherPane))
                } else {
                    None
                }
            ),
        ]
        .spacing(1)
        .align_y(iced::Alignment::Center);

        let right_group: Element<'_, Message> = row![view_buttons, cross_buttons]
            .spacing(4)
            .align_y(iced::Alignment::Center)
            .into();

        let pane_toolbar_width = pane_available_width(state, pane_idx);

        let pane_toolbar: Element<'_, Message> = mouse_area(toolbar_wrap(
            nav_buttons.into(),
            action_buttons.into(),
            right_group,
            4.0,
            [2, 4],
            pane_toolbar_width,
        ))
        .on_press(Message::SetActivePane(pane_idx))
        .into();

        column![
            tab_bar,
            pane_toolbar,
            breadcrumb,
            filter_bar,
            pane_drop_area
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    } else {
        column![tab_bar, breadcrumb, filter_bar, pane_drop_area]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

fn pane_state_placeholder(icon: &'static str, label: String) -> Element<'static, Message> {
    container(
        column![
            crate::views::theme::icon(icon)
                .size(38)
                .style(crate::views::theme::muted_text_style),
            text(label)
                .size(13)
                .style(crate::views::theme::muted_text_style),
        ]
        .spacing(10)
        .align_x(iced::Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .center(Length::Fill)
    .into()
}
