use crate::messages::{Message, PaneMessage};
use crate::runtime::app_runtime;
use crate::state::{AppState, PaneLoadState, PaneState, PathSuggestion, SortOrder, ViewMode};
use crate::update::file_ops::open_object_task;
use crate::update::helpers::*;
use crate::update::metadata_hydration::{
    apply_metadata_hydration, schedule_visible_metadata_hydration,
};
use crate::update::preview::refresh_preview_for_selection;
use crate::update::thumbnail::*;
use crate::views::filter::map_panel::{visible_tiles, MapViewport, TileCoord};
use crate::widgets::metrics::{
    GRID_FILE_ROW_HEIGHT, GRID_FOLDER_ROW_HEIGHT, GRID_SECTION_SEPARATOR_HEIGHT, LIST_HEADER_HEIGHT,
};
use chrono::{Datelike, Months, NaiveDate, TimeZone, Utc};
use iced::Task;
use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::object::ObjectRef;
use iris_search::filter::Scope;
use iris_search::filter::{GpsBounds, GpsFilter, RangeFilter};
use iris_search::token::format_filter_tokens;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

const DOUBLE_CLICK_THRESHOLD_MS: u64 = 400;
const PATH_SUGGESTION_LIMIT: usize = 8;
pub(super) const MAP_TILE_RETRY_BACKOFF: Duration = Duration::from_secs(15);

/// Moves focus; with Shift held the selection extends from the anchor, otherwise it follows focus.
fn move_focus(
    state: &mut AppState,
    pane_idx: usize,
    step: impl FnOnce(&mut PaneState),
) -> Task<Message> {
    let extend = state.keyboard_modifiers.shift();
    let command = state.keyboard_modifiers.command();
    let pane = state.pane_mut(pane_idx);
    let anchor = pane
        .selection_anchor_index()
        .or(pane.focused_index)
        .or_else(|| pane.selected_indices.iter().copied().min());
    step(pane);
    let Some(index) = pane.focused_index else {
        return Task::none();
    };
    if extend {
        pane.select_range_from(anchor.unwrap_or(index), index, command);
    } else {
        pane.select_single(index);
    }
    refresh_preview_and_reveal(state, pane_idx)
}

fn refresh_preview_and_reveal(state: &mut AppState, pane_idx: usize) -> Task<Message> {
    let preview = refresh_preview_for_selection(state);
    let pane = state.pane(pane_idx);
    let Some(index) = pane.focused_index else {
        return preview;
    };
    let items_per_row = grid_items_per_row_for_pane(state, pane_idx).max(1);
    let (target_y, target_height) = match pane.view_mode {
        ViewMode::List => {
            let Some(position) = pane.visible_position(index) else {
                return preview;
            };
            (
                LIST_HEADER_HEIGHT + pane.list_row_offset(position),
                pane.list_row_offset(position + 1) - pane.list_row_offset(position),
            )
        }
        ViewMode::Grid => {
            if let Some(position) = pane
                .visible_folder_indices()
                .iter()
                .position(|candidate| *candidate == index)
            {
                (
                    (position / items_per_row) as f32 * GRID_FOLDER_ROW_HEIGHT,
                    GRID_FOLDER_ROW_HEIGHT,
                )
            } else if let Some(position) = pane
                .visible_file_indices()
                .iter()
                .position(|candidate| *candidate == index)
            {
                let folder_rows = pane.visible_folder_indices().len().div_ceil(items_per_row);
                let separator = if folder_rows > 0 {
                    GRID_SECTION_SEPARATOR_HEIGHT
                } else {
                    0.0
                };
                (
                    folder_rows as f32 * GRID_FOLDER_ROW_HEIGHT
                        + separator
                        + (position / items_per_row) as f32 * GRID_FILE_ROW_HEIGHT,
                    GRID_FILE_ROW_HEIGHT,
                )
            } else {
                return preview;
            }
        }
    };
    let viewport_height = pane.viewport_height.max(1.0);
    if target_y >= pane.scroll_y && target_y + target_height <= pane.scroll_y + viewport_height {
        return preview;
    }
    let y = (target_y - (viewport_height - target_height).max(0.0) / 2.0).max(0.0);
    let reveal = iced::widget::operation::scroll_to(
        crate::app::ids::pane_scroll_id(pane_idx),
        iced::widget::scrollable::AbsoluteOffset { x: 0.0, y },
    );
    Task::batch([preview, reveal])
}

pub fn handle_pane_message(
    state: &mut AppState,
    pane_idx: usize,
    msg: PaneMessage,
) -> Task<Message> {
    match msg {
        PaneMessage::NavigateTo(index) => {
            if let Some(entry) = state.pane(pane_idx).entry(index) {
                if entry.is_dir {
                    let new_path = format!("{}/{}", state.pane(pane_idx).current_path, entry.name);
                    return navigate_pane_to_path(state, pane_idx, new_path);
                }
                state.pane_mut(pane_idx).select_single(index);
            }
        }

        PaneMessage::NavigateToPath(path) => {
            return navigate_pane_to_path(state, pane_idx, path);
        }

        PaneMessage::NavigateToBreadcrumb(index) => {
            if let Some(segment) = state.pane(pane_idx).breadcrumb_segments.get(index) {
                let path = segment.full_path.clone();
                return navigate_pane_to_path(state, pane_idx, path);
            }
        }

        PaneMessage::DirectoryHeadLoaded { path, entries } => {
            return apply_directory_entries(state, pane_idx, path, entries, true);
        }

        PaneMessage::DirectoryLoaded { path, entries } => {
            return apply_directory_entries(state, pane_idx, path, entries, false);
        }

        PaneMessage::DirectoryBatchLoaded {
            path,
            entries,
            done,
        } => {
            return apply_directory_entry_batch(state, pane_idx, path, entries, done);
        }

        PaneMessage::MetadataHydrated(updates) => {
            return apply_metadata_hydration(state, pane_idx, updates);
        }

        PaneMessage::DirectoryLoadFailed { path, error } => {
            if state.pane(pane_idx).current_path != path {
                return Task::none();
            }
            let pane = state.pane_mut(pane_idx);
            pane.clear_entries();
            pane.metadata_hydration_inflight.clear();
            pane.selected_indices.clear();
            pane.focused_index = None;
            pane.load_state = PaneLoadState::Failed { message: error };
            if pane_idx == state.active_pane {
                return refresh_preview_and_reveal(state, pane_idx);
            }
        }

        PaneMessage::SelectItem(index) => {
            state.active_pane = pane_idx;
            let now = std::time::Instant::now();
            let is_double_click = state.pane(pane_idx).last_click_index == Some(index)
                && state
                    .pane(pane_idx)
                    .last_click_at
                    .map(|t| {
                        now.duration_since(t) <= Duration::from_millis(DOUBLE_CLICK_THRESHOLD_MS)
                    })
                    .unwrap_or(false);

            let shift = state.keyboard_modifiers.shift();
            let command = state.keyboard_modifiers.command();
            {
                let pane = state.pane_mut(pane_idx);
                pane.last_click_index = Some(index);
                pane.last_click_at = Some(now);

                if shift {
                    pane.select_range_to(index, command);
                } else if command {
                    pane.toggle_selection(index);
                } else {
                    pane.select_single(index);
                }
            }

            let preview_task = refresh_preview_for_selection(state);

            if is_double_click {
                if let Some(entry) = state.pane(pane_idx).entry(index) {
                    let entry_name = entry.name.clone();
                    let is_dir = entry.is_dir;
                    if is_dir {
                        let new_path =
                            format!("{}/{}", state.pane(pane_idx).current_path, entry_name);
                        return navigate_pane_to_path(state, pane_idx, new_path);
                    }
                    return Task::batch([preview_task, open_object_task(entry.object_ref.clone())]);
                }
            }

            return preview_task;
        }

        PaneMessage::ToggleSelection(index) => {
            state.active_pane = pane_idx;
            state.pane_mut(pane_idx).toggle_selection(index);
            return refresh_preview_and_reveal(state, pane_idx);
        }

        PaneMessage::SelectAll => {
            state.pane_mut(pane_idx).select_all();
            return refresh_preview_and_reveal(state, pane_idx);
        }

        PaneMessage::ClearSelection => {
            state.pane_mut(pane_idx).clear_selection();
            return refresh_preview_and_reveal(state, pane_idx);
        }

        PaneMessage::FocusNext => return move_focus(state, pane_idx, |pane| pane.focus_next()),

        PaneMessage::FocusPrevious => {
            return move_focus(state, pane_idx, |pane| pane.focus_previous());
        }

        PaneMessage::FocusNextRow => {
            let items_per_row = grid_items_per_row_for_pane(state, pane_idx);
            return move_focus(state, pane_idx, |pane| pane.focus_next_row(items_per_row));
        }

        PaneMessage::FocusPreviousRow => {
            let items_per_row = grid_items_per_row_for_pane(state, pane_idx);
            return move_focus(state, pane_idx, |pane| {
                pane.focus_previous_row(items_per_row)
            });
        }

        PaneMessage::ActivateFocused => {
            if let Some(index) = state.pane(pane_idx).focused_index {
                if let Some(entry) = state.pane(pane_idx).entry(index) {
                    let entry_name = entry.name.clone();
                    let object_ref = entry.object_ref.clone();
                    if entry.is_dir {
                        let new_path =
                            format!("{}/{}", state.pane(pane_idx).current_path, entry_name);
                        return navigate_pane_to_path(state, pane_idx, new_path);
                    }
                    state.pane_mut(pane_idx).select_single(index);
                    let preview_task = refresh_preview_for_selection(state);
                    return Task::batch([preview_task, open_object_task(object_ref)]);
                }
            }
        }

        PaneMessage::SetViewMode(mode) => {
            if state.pane(pane_idx).view_mode == mode {
                return Task::none();
            }
            state.pane_mut(pane_idx).view_mode = mode;
            match state.pane(pane_idx).view_mode {
                ViewMode::Grid => {
                    prepare_grid_view(state, pane_idx);
                    sync_thumbnail_generation_queue(state);
                    let thumbnail_task = generate_thumbnails(state);
                    let metadata_task = schedule_visible_metadata_hydration(state, pane_idx);
                    return Task::batch([thumbnail_task, metadata_task]);
                }
                ViewMode::List => {
                    stop_thumbnail_work(state);
                    return schedule_visible_metadata_hydration(state, pane_idx);
                }
            }
        }

        PaneMessage::SetSortKey(key) => {
            let pane = state.pane_mut(pane_idx);
            if pane.sort_key == key {
                pane.sort_order = if pane.sort_order == SortOrder::Ascending {
                    SortOrder::Descending
                } else {
                    SortOrder::Ascending
                };
            } else {
                pane.sort_key = key;
                pane.sort_order = SortOrder::Ascending;
            }
            pane.sort_entries();
            return schedule_visible_metadata_hydration(state, pane_idx);
        }

        PaneMessage::ToggleSortOrder => {
            let pane = state.pane_mut(pane_idx);
            pane.sort_order = match pane.sort_order {
                SortOrder::Ascending => SortOrder::Descending,
                SortOrder::Descending => SortOrder::Ascending,
            };
            pane.sort_entries();
            return schedule_visible_metadata_hydration(state, pane_idx);
        }

        PaneMessage::SetFilter(query) => {
            let (
                path,
                show_hidden,
                model,
                run_id,
                cancellation_token,
                should_run_recursive,
                collect_gps_points,
            ) = {
                let show_hidden = state.show_hidden_files;
                let pane = state.pane_mut(pane_idx);
                pane.set_filter_query(query);
                pane.scroll_y = 0.0;
                let should_run_recursive = pane.is_filter_active()
                    && pane.filter_model.scope == Scope::Recursive
                    && !app_runtime().is_virtual_display_path(&pane.current_path);
                let collect_gps_points = pane.filter_gps_picker_open;
                if should_run_recursive {
                    let (run_id, cancellation_token) = pane.begin_recursive_filter();
                    (
                        pane.current_path.clone(),
                        show_hidden,
                        pane.filter_model.clone(),
                        run_id,
                        Some(cancellation_token),
                        true,
                        collect_gps_points,
                    )
                } else {
                    pane.stop_recursive_filter();
                    (
                        String::new(),
                        show_hidden,
                        pane.filter_model.clone(),
                        pane.filter_run_id,
                        None,
                        false,
                        false,
                    )
                }
            };
            if should_run_recursive {
                let cancellation_token =
                    cancellation_token.expect("recursive filter has a cancellation token");
                return Task::batch([
                    refresh_preview_for_selection(state),
                    recursive_filter_task(
                        pane_idx,
                        run_id,
                        path,
                        show_hidden,
                        model,
                        cancellation_token,
                        collect_gps_points,
                    ),
                ]);
            }
            let preview_task = refresh_preview_for_selection(state);
            let metadata_task = schedule_visible_metadata_hydration(state, pane_idx);
            return Task::batch([preview_task, metadata_task]);
        }

        PaneMessage::PathInputChanged(value) => {
            state.pane_mut(pane_idx).path_input = value;
            refresh_path_suggestions(state, pane_idx);
        }

        PaneMessage::CommitPathEdit => {
            let (input, current_path) = {
                let pane = state.pane(pane_idx);
                let selected = pane
                    .path_suggestion_selected
                    .and_then(|index| pane.path_suggestions.get(index))
                    .map(|suggestion| suggestion.value.clone());
                (
                    selected.unwrap_or_else(|| pane.path_input.trim().to_string()),
                    pane.current_path.clone(),
                )
            };
            if input.is_empty() {
                return Task::none();
            }
            let Some(path) = app_runtime().resolve_navigation_input(&input, &current_path) else {
                return Task::none();
            };
            {
                let pane = state.pane_mut(pane_idx);
                pane.path_editing = false;
                pane.path_input.clear();
                pane.path_suggestions.clear();
                pane.path_suggestion_selected = None;
            }
            return navigate_pane_to_path(state, pane_idx, path);
        }

        PaneMessage::CancelPathEdit => {
            let pane = state.pane_mut(pane_idx);
            pane.path_editing = false;
            pane.path_input.clear();
            pane.path_suggestions.clear();
            pane.path_suggestion_selected = None;
        }

        PaneMessage::PathSuggestionNext => {
            move_path_suggestion_selection(state, pane_idx, 1);
        }

        PaneMessage::PathSuggestionPrevious => {
            move_path_suggestion_selection(state, pane_idx, -1);
        }

        PaneMessage::AcceptPathSuggestion => {
            accept_path_suggestion(state, pane_idx);
        }

        PaneMessage::CommitPathSuggestion(index) => {
            let (input, current_path) = {
                let pane = state.pane(pane_idx);
                let Some(suggestion) = pane.path_suggestions.get(index) else {
                    return Task::none();
                };
                (suggestion.value.clone(), pane.current_path.clone())
            };
            let Some(path) = app_runtime().resolve_navigation_input(&input, &current_path) else {
                return Task::none();
            };
            {
                let pane = state.pane_mut(pane_idx);
                pane.path_editing = false;
                pane.path_input.clear();
                pane.path_suggestions.clear();
                pane.path_suggestion_selected = None;
            }
            return navigate_pane_to_path(state, pane_idx, path);
        }

        PaneMessage::AppendFilterToken(token) => {
            let query = {
                let current = state.pane(pane_idx).filter_query.trim();
                if current.is_empty() {
                    token
                } else {
                    format!("{current} {token}")
                }
            };
            return handle_pane_message(state, pane_idx, PaneMessage::SetFilter(query));
        }

        PaneMessage::ToggleDateRangePicker => {
            let pane = state.pane_mut(pane_idx);
            pane.filter_date_picker_open = !pane.filter_date_picker_open;
            if pane.filter_date_picker_open {
                sync_date_picker_inputs_from_model(pane);
                sync_date_anchor_from_inputs(pane);
            }
            return Task::none();
        }

        PaneMessage::SetDateRangeStart(value) => {
            state.pane_mut(pane_idx).filter_date_start_input = value;
            return Task::none();
        }

        PaneMessage::SetDateRangeEnd(value) => {
            state.pane_mut(pane_idx).filter_date_end_input = value;
            return Task::none();
        }

        PaneMessage::ApplyDateRange => {
            let Some(query) = apply_date_range_query(state.pane(pane_idx)) else {
                return Task::none();
            };
            return handle_pane_message(state, pane_idx, PaneMessage::SetFilter(query));
        }

        PaneMessage::ClearDateRange => {
            let query = {
                let pane = state.pane_mut(pane_idx);
                pane.filter_date_start_input.clear();
                pane.filter_date_end_input.clear();
                pane.filter_date_anchor = Some(today_date());
                let mut model = pane.filter_model.clone();
                model.date = None;
                format_filter_tokens(&model)
            };
            return handle_pane_message(state, pane_idx, PaneMessage::SetFilter(query));
        }

        PaneMessage::SelectDateRangeDay(value) => {
            let Some(date) = parse_date_only(&value) else {
                return Task::none();
            };
            let pane = state.pane_mut(pane_idx);
            let selection = date_selection_from_inputs(pane).click(date).normalized();
            apply_date_selection_to_inputs(pane, selection);
            return Task::none();
        }

        PaneMessage::PreviousDateRangeMonth => {
            shift_date_anchor(state.pane_mut(pane_idx), -1);
            return Task::none();
        }

        PaneMessage::NextDateRangeMonth => {
            shift_date_anchor(state.pane_mut(pane_idx), 1);
            return Task::none();
        }

        PaneMessage::PresetDateToday => {
            let pane = state.pane_mut(pane_idx);
            let today = today_date();
            pane.filter_date_anchor = Some(today);
            apply_date_selection_to_inputs(
                pane,
                crate::views::filter::date_range::preset_today(today),
            );
            return Task::none();
        }

        PaneMessage::PresetDateMonth => {
            let pane = state.pane_mut(pane_idx);
            let today = today_date();
            pane.filter_date_anchor = Some(today);
            apply_date_selection_to_inputs(
                pane,
                crate::views::filter::date_range::preset_month(today),
            );
            return Task::none();
        }

        PaneMessage::PresetDateYear => {
            let pane = state.pane_mut(pane_idx);
            let today = today_date();
            pane.filter_date_anchor = Some(today);
            apply_date_selection_to_inputs(
                pane,
                crate::views::filter::date_range::preset_year(today),
            );
            return Task::none();
        }

        PaneMessage::ToggleGpsBoundsPicker => {
            let pane = state.pane_mut(pane_idx);
            pane.filter_gps_picker_open = !pane.filter_gps_picker_open;
            if pane.filter_gps_picker_open {
                sync_gps_picker_inputs_from_model(pane);
            }
            return schedule_initial_map_tile_fetches(state, pane_idx);
        }

        PaneMessage::SetGpsMinLat(value) => {
            state.pane_mut(pane_idx).filter_gps_min_lat_input = value;
            return Task::none();
        }

        PaneMessage::SetGpsMaxLat(value) => {
            state.pane_mut(pane_idx).filter_gps_max_lat_input = value;
            return Task::none();
        }

        PaneMessage::SetGpsMinLon(value) => {
            state.pane_mut(pane_idx).filter_gps_min_lon_input = value;
            return Task::none();
        }

        PaneMessage::SetGpsMaxLon(value) => {
            state.pane_mut(pane_idx).filter_gps_max_lon_input = value;
            return Task::none();
        }

        PaneMessage::ApplyGpsBounds => {
            let Some(query) = apply_gps_bounds_query(state.pane(pane_idx)) else {
                return Task::none();
            };
            return handle_pane_message(state, pane_idx, PaneMessage::SetFilter(query));
        }

        PaneMessage::ApplyGpsBoundsFromMap {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        } => {
            let pane = state.pane_mut(pane_idx);
            pane.filter_gps_min_lat_input = format_gps_input(min_lat);
            pane.filter_gps_max_lat_input = format_gps_input(max_lat);
            pane.filter_gps_min_lon_input = format_gps_input(min_lon);
            pane.filter_gps_max_lon_input = format_gps_input(max_lon);
            let Some(query) = apply_gps_bounds_query(state.pane(pane_idx)) else {
                return Task::none();
            };
            return handle_pane_message(state, pane_idx, PaneMessage::SetFilter(query));
        }

        PaneMessage::GpsMapVisibleTiles(tiles) => {
            return schedule_map_tile_fetches(state, pane_idx, tiles);
        }

        PaneMessage::GpsMapTileLoaded { tile, result } => {
            let pane = state.pane_mut(pane_idx);
            pane.filter_map_tiles_loading.remove(&tile);
            match result {
                Ok(bytes) => {
                    pane.filter_map_tiles_failed.remove(&tile);
                    pane.filter_map_tiles
                        .insert(tile, iced::advanced::image::Handle::from_bytes(bytes));
                }
                Err(_) => {
                    pane.filter_map_tiles_failed
                        .insert(tile, std::time::Instant::now());
                }
            }
        }

        PaneMessage::ClearGpsBounds => {
            let query = {
                let pane = state.pane_mut(pane_idx);
                clear_gps_picker_inputs(pane);
                let mut model = pane.filter_model.clone();
                model.gps = GpsFilter::Any;
                format_filter_tokens(&model)
            };
            return handle_pane_message(state, pane_idx, PaneMessage::SetFilter(query));
        }

        PaneMessage::RecursiveFilterBatch {
            run_id,
            entries,
            scanned,
            matched,
            done,
        } => {
            if state.pane(pane_idx).filter_run_id != run_id {
                return Task::none();
            }
            {
                let pane = state.pane_mut(pane_idx);
                pane.filter_recursive_scanned = scanned;
                pane.filter_recursive_matched = matched;
                pane.append_entries(entries);
                pane.load_state = if done {
                    PaneLoadState::Loaded
                } else {
                    PaneLoadState::LoadingMore
                };
                pane.filter_recursive_running = !done;
                if done {
                    pane.filter_cancellation_token = None;
                }
                pane.focused_index = pane.first_visible_index();
            }
            prune_thumbnail_cache(state);
            seed_thumbnail_generation_queue(state);
            let thumbnail_task = generate_thumbnails(state);
            let preview_task = if pane_idx == state.active_pane {
                refresh_preview_for_selection(state)
            } else {
                Task::none()
            };
            return Task::batch([thumbnail_task, preview_task]);
        }

        PaneMessage::RecursiveFilterProgress {
            run_id,
            scanned,
            matched,
        } => {
            if state.pane(pane_idx).filter_run_id == run_id {
                let pane = state.pane_mut(pane_idx);
                pane.filter_recursive_scanned = scanned;
                pane.filter_recursive_matched = matched;
            }
        }

        PaneMessage::RecursiveFilterGpsPoints { run_id, points } => {
            if state.pane(pane_idx).filter_run_id == run_id {
                state.pane_mut(pane_idx).filter_gps_points = points;
            }
        }

        PaneMessage::RecursiveFilterFailed { run_id, error } => {
            if state.pane(pane_idx).filter_run_id == run_id {
                let pane = state.pane_mut(pane_idx);
                pane.clear_entries();
                pane.selected_indices.clear();
                pane.focused_index = None;
                pane.filter_recursive_running = false;
                pane.filter_cancellation_token = None;
                pane.filter_gps_points.clear();
                pane.load_state = PaneLoadState::Failed { message: error };
                if pane_idx == state.active_pane {
                    return refresh_preview_and_reveal(state, pane_idx);
                }
            }
        }

        PaneMessage::CancelRecursiveFilter => {
            state.pane_mut(pane_idx).stop_recursive_filter();
        }

        PaneMessage::LoadMoreItems => {}

        PaneMessage::Scrolled {
            offset_y,
            viewport_height,
        } => {
            let offset_y = offset_y.max(0.0);
            let viewport_height = viewport_height.max(0.0);
            let pane = state.pane_mut(pane_idx);
            if pane.update_viewport(offset_y, viewport_height) {
                return schedule_visible_metadata_hydration(state, pane_idx);
            }
        }

        PaneMessage::GridScrolled {
            offset_y,
            viewport_height,
        } => {
            let offset_y = offset_y.max(0.0);
            let viewport_height = viewport_height.max(0.0);
            let (view_mode, changed) = {
                let pane = state.pane_mut(pane_idx);
                let changed = pane.update_viewport(offset_y, viewport_height);
                (pane.view_mode.clone(), changed)
            };
            let mut tasks = Vec::new();
            if changed {
                tasks.push(schedule_visible_metadata_hydration(state, pane_idx));
            }
            if view_mode == ViewMode::Grid && changed {
                sync_thumbnail_generation_queue(state);
                tasks.push(generate_thumbnails(state));
            }
            if !tasks.is_empty() {
                return Task::batch(tasks);
            }
        }

        PaneMessage::InlineRenameInputChanged { index, value } => {
            state
                .pane_mut(pane_idx)
                .update_inline_rename_input(index, value);
        }

        PaneMessage::CommitInlineRename(index) => {
            let Some(mut rename) = state.pane_mut(pane_idx).take_inline_rename_for(index) else {
                return Task::none();
            };
            let new_name = rename.input_value.clone();
            if new_name == rename.original_name {
                return Task::none();
            }
            if let Some(message) = crate::format::file_name_error(&new_name) {
                rename.error_message = Some(message);
                let pane = state.pane_mut(pane_idx);
                pane.inline_rename = Some(rename);
                pane.rebuild_list_row_offsets();
                return Task::none();
            }

            return rename_object_task(rename.target, new_name);
        }

        PaneMessage::CancelInlineRename => {
            state.pane_mut(pane_idx).cancel_inline_rename();
        }
    }
    Task::none()
}

fn rename_object_task(target: ObjectRef, new_name: String) -> Task<Message> {
    let original_path = PathBuf::from(&target.display_path);
    let renamed_path = original_path
        .parent()
        .map(|parent| parent.join(&new_name))
        .unwrap_or_else(|| PathBuf::from(&new_name));
    Task::perform(
        async move { app_runtime().rename_object(&target, &new_name).await },
        move |result| Message::RenameCompleted {
            original_path: original_path.clone(),
            renamed_path,
            result,
        },
    )
}

fn schedule_initial_map_tile_fetches(state: &mut AppState, pane_idx: usize) -> Task<Message> {
    if !state.map_tile_fetch_enabled || !state.pane(pane_idx).filter_gps_picker_open {
        return Task::none();
    }
    let pane = state.pane(pane_idx);
    let bounds = parse_map_bounds_from_gps_inputs(pane)
        .or_else(|| {
            let points = pane
                .filter_gps_points
                .iter()
                .map(|(lat, lon)| crate::views::filter::map_panel::LatLon {
                    lat: f64::from(*lat),
                    lon: f64::from(*lon),
                })
                .collect::<Vec<_>>();
            crate::views::filter::map_panel::fit_bounds(&points)
        })
        .unwrap_or(crate::views::filter::map_panel::GpsBounds {
            min_lat: -85.0,
            max_lat: 85.0,
            min_lon: -180.0,
            max_lon: 180.0,
        });
    let viewport = MapViewport::fit_bounds(bounds, 360.0, 140.0, 18.0);
    schedule_map_tile_fetches(state, pane_idx, visible_tiles(viewport, 360.0, 140.0))
}

fn schedule_map_tile_fetches(
    state: &mut AppState,
    pane_idx: usize,
    tiles: Vec<TileCoord>,
) -> Task<Message> {
    if !state.map_tile_fetch_enabled || !state.pane(pane_idx).filter_gps_picker_open {
        return Task::none();
    }
    let mut tasks = Vec::new();
    {
        let pane = state.pane_mut(pane_idx);
        for tile in tiles {
            let retry_blocked = pane
                .filter_map_tiles_failed
                .get(&tile)
                .is_some_and(|failed_at| failed_at.elapsed() < MAP_TILE_RETRY_BACKOFF);
            if pane.filter_map_tiles.contains_key(&tile)
                || pane.filter_map_tiles_loading.contains(&tile)
                || retry_blocked
            {
                continue;
            }
            pane.filter_map_tiles_failed.remove(&tile);
            pane.filter_map_tiles_loading.insert(tile);
            tasks.push(Task::perform(
                async move { (tile, app_runtime().fetch_map_tile(tile).await) },
                move |(tile, result)| {
                    Message::Pane(pane_idx, PaneMessage::GpsMapTileLoaded { tile, result })
                },
            ));
        }
    }
    Task::batch(tasks)
}

fn sync_date_picker_inputs_from_model(pane: &mut crate::state::PaneState) {
    if let Some(date) = &pane.filter_model.date {
        pane.filter_date_start_input = date
            .min
            .and_then(format_date_input_from_timestamp)
            .unwrap_or_default();
        pane.filter_date_end_input = date
            .max
            .and_then(format_date_input_from_timestamp)
            .unwrap_or_default();
    } else {
        pane.filter_date_start_input.clear();
        pane.filter_date_end_input.clear();
    }
}

fn sync_date_anchor_from_inputs(pane: &mut crate::state::PaneState) {
    pane.filter_date_anchor = date_selection_from_inputs(pane)
        .start
        .or_else(|| Some(today_date()));
}

fn date_selection_from_inputs(
    pane: &crate::state::PaneState,
) -> crate::views::filter::date_range::DateRangeSelection {
    crate::views::filter::date_range::DateRangeSelection {
        start: parse_date_only(&pane.filter_date_start_input),
        end: parse_date_only(&pane.filter_date_end_input),
    }
    .normalized()
}

fn apply_date_selection_to_inputs(
    pane: &mut crate::state::PaneState,
    selection: crate::views::filter::date_range::DateRangeSelection,
) {
    let selection = selection.normalized();
    pane.filter_date_start_input = selection.start.map(format_date_input).unwrap_or_default();
    pane.filter_date_end_input = selection.end.map(format_date_input).unwrap_or_default();
    pane.filter_date_anchor = selection.start.or(pane.filter_date_anchor);
}

fn shift_date_anchor(pane: &mut crate::state::PaneState, months: i32) {
    let anchor = pane.filter_date_anchor.unwrap_or_else(today_date);
    let first = NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1).expect("valid month");
    pane.filter_date_anchor = if months < 0 {
        first.checked_sub_months(Months::new(months.unsigned_abs()))
    } else {
        first.checked_add_months(Months::new(months as u32))
    };
}

fn apply_date_range_query(pane: &crate::state::PaneState) -> Option<String> {
    let start = pane.filter_date_start_input.trim();
    let end = pane.filter_date_end_input.trim();
    let min = if start.is_empty() {
        None
    } else {
        Some(parse_date_start_input(start)?)
    };
    let max = if end.is_empty() {
        None
    } else {
        Some(parse_date_end_input(end)?)
    };

    let mut model = pane.filter_model.clone();
    model.date = (min.is_some() || max.is_some()).then_some(RangeFilter { min, max });
    Some(format_filter_tokens(&model))
}

fn parse_date_only(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok()
}

fn format_date_input(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

fn today_date() -> NaiveDate {
    chrono::Local::now().date_naive()
}

fn sync_gps_picker_inputs_from_model(pane: &mut crate::state::PaneState) {
    if let GpsFilter::Bounds(bounds) = pane.filter_model.gps {
        pane.filter_gps_min_lat_input = format_gps_input(bounds.min_lat);
        pane.filter_gps_max_lat_input = format_gps_input(bounds.max_lat);
        pane.filter_gps_min_lon_input = format_gps_input(bounds.min_lon);
        pane.filter_gps_max_lon_input = format_gps_input(bounds.max_lon);
    } else {
        clear_gps_picker_inputs(pane);
    }
}

fn clear_gps_picker_inputs(pane: &mut crate::state::PaneState) {
    pane.filter_gps_min_lat_input.clear();
    pane.filter_gps_max_lat_input.clear();
    pane.filter_gps_min_lon_input.clear();
    pane.filter_gps_max_lon_input.clear();
}

fn apply_gps_bounds_query(pane: &crate::state::PaneState) -> Option<String> {
    let min_lat = parse_lat_input(&pane.filter_gps_min_lat_input)?;
    let max_lat = parse_lat_input(&pane.filter_gps_max_lat_input)?;
    let min_lon = parse_lon_input(&pane.filter_gps_min_lon_input)?;
    let max_lon = parse_lon_input(&pane.filter_gps_max_lon_input)?;
    if min_lat > max_lat {
        return None;
    }

    let mut model = pane.filter_model.clone();
    model.gps = GpsFilter::Bounds(GpsBounds {
        min_lat,
        max_lat,
        min_lon,
        max_lon,
    });
    Some(format_filter_tokens(&model))
}

fn parse_map_bounds_from_gps_inputs(
    pane: &crate::state::PaneState,
) -> Option<crate::views::filter::map_panel::GpsBounds> {
    let min_lat = f64::from(parse_lat_input(&pane.filter_gps_min_lat_input)?);
    let max_lat = f64::from(parse_lat_input(&pane.filter_gps_max_lat_input)?);
    let min_lon = f64::from(parse_lon_input(&pane.filter_gps_min_lon_input)?);
    let max_lon = f64::from(parse_lon_input(&pane.filter_gps_max_lon_input)?);
    (min_lat <= max_lat).then_some(crate::views::filter::map_panel::GpsBounds {
        min_lat,
        max_lat,
        min_lon,
        max_lon,
    })
}

fn parse_lat_input(value: &str) -> Option<f32> {
    parse_bounded_gps_input(value, -90.0, 90.0)
}

fn parse_lon_input(value: &str) -> Option<f32> {
    parse_bounded_gps_input(value, -180.0, 180.0)
}

fn parse_bounded_gps_input(value: &str, min: f32, max: f32) -> Option<f32> {
    let value = value.trim().parse::<f32>().ok()?;
    value
        .is_finite()
        .then_some(value)?
        .clamp(min, max)
        .eq(&value)
        .then_some(value)
}

fn format_gps_input(value: f32) -> String {
    let mut text = format!("{value:.6}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

fn parse_date_start_input(value: &str) -> Option<i64> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    Some(
        Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?)
            .timestamp(),
    )
}

fn parse_date_end_input(value: &str) -> Option<i64> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    Some(
        Utc.from_utc_datetime(&date.and_hms_opt(23, 59, 59)?)
            .timestamp(),
    )
}

fn format_date_input_from_timestamp(timestamp: i64) -> Option<String> {
    Utc.timestamp_opt(timestamp, 0)
        .single()
        .map(|datetime| datetime.date_naive().format("%Y-%m-%d").to_string())
}

fn apply_directory_entries(
    state: &mut AppState,
    pane_idx: usize,
    path: String,
    entries: Vec<iris_core_domain::types::list::ListEntry>,
    loading_full: bool,
) -> Task<Message> {
    if state.pane(pane_idx).current_path != path {
        return Task::none();
    }

    let previous_entries = state.pane(pane_idx).entries.clone();
    invalidate_changed_thumbnail_entries(state, &previous_entries, &entries);
    {
        let pane = state.pane_mut(pane_idx);
        pane.replace_entries(entries);
        pane.scroll_y = 0.0;
        pane.load_state = if loading_full {
            PaneLoadState::LoadingMore
        } else {
            PaneLoadState::Loaded
        };
        pane.focused_index = pane.first_visible_index();
    }

    prune_thumbnail_cache(state);
    seed_thumbnail_generation_queue(state);
    let thumbnail_task = generate_thumbnails(state);
    let preview_task = if pane_idx == state.active_pane {
        refresh_preview_for_selection(state)
    } else {
        Task::none()
    };
    let metadata_task = schedule_visible_metadata_hydration(state, pane_idx);
    Task::batch([thumbnail_task, preview_task, metadata_task])
}

fn apply_directory_entry_batch(
    state: &mut AppState,
    pane_idx: usize,
    path: String,
    entries: Vec<iris_core_domain::types::list::ListEntry>,
    done: bool,
) -> Task<Message> {
    if state.pane(pane_idx).current_path != path {
        return Task::none();
    }

    {
        let pane = state.pane_mut(pane_idx);
        pane.append_entries(entries);
        pane.load_state = if done {
            PaneLoadState::Loaded
        } else {
            PaneLoadState::LoadingMore
        };
        if pane.focused_index.is_none() {
            pane.focused_index = pane.first_visible_index();
        }
    }

    prune_thumbnail_cache(state);
    sync_thumbnail_generation_queue(state);
    let thumbnail_task = generate_thumbnails(state);
    let preview_task = if pane_idx == state.active_pane {
        refresh_preview_for_selection(state)
    } else {
        Task::none()
    };
    let metadata_task = schedule_visible_metadata_hydration(state, pane_idx);
    Task::batch([thumbnail_task, preview_task, metadata_task])
}

pub fn refresh_path_suggestions(state: &mut AppState, pane_idx: usize) {
    let suggestions = build_path_suggestions(state, pane_idx, PATH_SUGGESTION_LIMIT);
    let pane = state.pane_mut(pane_idx);
    pane.path_suggestions = suggestions;
    pane.path_suggestion_selected = None;
}

fn build_path_suggestions(state: &AppState, pane_idx: usize, limit: usize) -> Vec<PathSuggestion> {
    if limit == 0 {
        return Vec::new();
    }

    let pane = state.pane(pane_idx);
    let input = pane.path_input.trim();
    let current_path = pane.current_path.clone();
    let mut suggestions = Vec::new();
    let mut seen = HashSet::new();

    collect_entry_path_suggestions(
        input,
        &current_path,
        &pane.entries,
        &mut suggestions,
        &mut seen,
        limit,
    );
    collect_known_path_suggestions(
        input,
        &pane.navigation_history,
        &pane.breadcrumb_segments,
        &mut suggestions,
        &mut seen,
        limit,
    );
    if suggestions.len() < limit {
        for candidate in app_runtime().path_completion_candidates(input, &current_path, limit) {
            if push_path_suggestion(
                &mut suggestions,
                &mut seen,
                candidate.clone(),
                candidate,
                limit,
            ) {
                break;
            }
        }
    }

    suggestions
}

fn collect_entry_path_suggestions(
    input: &str,
    current_path: &str,
    entries: &[ListEntry],
    suggestions: &mut Vec<PathSuggestion>,
    seen: &mut HashSet<String>,
    limit: usize,
) {
    let (base, name_prefix) = split_path_completion_input(input);
    if !input_base_matches_current_path(&base, current_path) {
        return;
    }

    let query = name_prefix.to_lowercase();
    for entry in entries.iter().filter(|entry| entry.is_dir) {
        if !query.is_empty() && !entry.name.to_lowercase().starts_with(&query) {
            continue;
        }
        let value = join_path_completion_value(&base, &entry.name);
        if push_path_suggestion(suggestions, seen, value.clone(), value, limit) {
            return;
        }
    }
}

fn collect_known_path_suggestions(
    input: &str,
    history: &[String],
    breadcrumbs: &[crate::state::BreadcrumbSegment],
    suggestions: &mut Vec<PathSuggestion>,
    seen: &mut HashSet<String>,
    limit: usize,
) {
    let query = input.to_lowercase();
    if query.is_empty() {
        return;
    }

    for path in history
        .iter()
        .rev()
        .chain(breadcrumbs.iter().rev().map(|segment| &segment.full_path))
    {
        let display_path = app_runtime().format_display_path(path);
        if display_path.to_lowercase().starts_with(&query)
            && push_path_suggestion(suggestions, seen, display_path.clone(), display_path, limit)
        {
            return;
        }
    }
}

fn push_path_suggestion(
    suggestions: &mut Vec<PathSuggestion>,
    seen: &mut HashSet<String>,
    label: String,
    value: String,
    limit: usize,
) -> bool {
    if value.trim().is_empty() || !seen.insert(value.clone()) {
        return suggestions.len() >= limit;
    }
    suggestions.push(PathSuggestion { label, value });
    suggestions.len() >= limit
}

fn split_path_completion_input(input: &str) -> (String, String) {
    let trimmed = input.trim();
    if let Some((base, name)) = trimmed.rsplit_once('/') {
        let base = if base.is_empty() {
            "/".to_string()
        } else {
            format!("{base}/")
        };
        (base, name.to_string())
    } else {
        (String::new(), trimmed.to_string())
    }
}

fn input_base_matches_current_path(base: &str, current_path: &str) -> bool {
    if base.is_empty() {
        return true;
    }
    let base_without_trailing = base.trim_end_matches('/');
    let base_input = if base_without_trailing.is_empty() {
        "/"
    } else {
        base_without_trailing
    };
    app_runtime()
        .resolve_navigation_input(base_input, current_path)
        .is_some_and(|path| path == current_path)
}

fn join_path_completion_value(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

fn move_path_suggestion_selection(state: &mut AppState, pane_idx: usize, direction: i8) {
    let pane = state.pane_mut(pane_idx);
    let len = pane.path_suggestions.len();
    if len == 0 {
        pane.path_suggestion_selected = None;
        return;
    }

    pane.path_suggestion_selected = Some(match (pane.path_suggestion_selected, direction) {
        (Some(index), 1) => (index + 1) % len,
        (Some(0), -1) | (None, -1) => len - 1,
        (Some(index), -1) => index - 1,
        (Some(index), _) => index,
        (None, _) => 0,
    });
}

fn accept_path_suggestion(state: &mut AppState, pane_idx: usize) {
    let value = {
        let pane = state.pane(pane_idx);
        pane.path_suggestion_selected
            .and_then(|index| pane.path_suggestions.get(index))
            .or_else(|| pane.path_suggestions.first())
            .map(|suggestion| suggestion.value.clone())
    };
    if let Some(value) = value {
        state.pane_mut(pane_idx).path_input = value;
        refresh_path_suggestions(state, pane_idx);
    }
}

/// Pane 0 loads progressively, pane 1 loads in full.
pub(crate) fn navigate_pane_to_path(
    state: &mut AppState,
    pane_idx: usize,
    path: String,
) -> Task<Message> {
    if pane_idx == 0 {
        navigate_to_path(state, path)
    } else {
        let canonical = if app_runtime().is_virtual_display_path(&path) {
            path
        } else {
            std::path::Path::new(&path)
                .canonicalize()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or(path)
        };
        {
            let pane = state.pane_mut(1);
            pane.current_path = canonical.clone();
            pane.push_to_history(canonical.clone());
        }
        reset_pane_directory_state(state, 1);
        state.pane_mut(1).load_state = PaneLoadState::LoadingInitial;
        refresh_breadcrumbs_for_pane(state, 1);
        load_secondary_directory_task(canonical)
    }
}
