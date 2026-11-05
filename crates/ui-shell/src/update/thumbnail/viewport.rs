use super::*;

use crate::widgets::metrics::{
    GRID_FILE_ROW_HEIGHT as GRID_FILE_ROW_ESTIMATED_HEIGHT,
    GRID_FOLDER_ROW_HEIGHT as GRID_FOLDER_ROW_ESTIMATED_HEIGHT,
    GRID_SECTION_SEPARATOR_HEIGHT as GRID_SECTION_SEPARATOR_ESTIMATED_HEIGHT,
    LIST_HEADER_HEIGHT as LIST_HEADER_ESTIMATED_HEIGHT,
};

pub fn prepare_grid_view(state: &mut AppState, pane_idx: usize) {
    let total = state.pane(pane_idx).entries.len();
    if total == 0 {
        state.pane_mut(pane_idx).scroll_y = 0.0;
        return;
    }

    let items_per_row = grid_items_per_row_for_pane(state, pane_idx).max(1);
    let anchor_index = state
        .pane(pane_idx)
        .focused_index
        .or_else(|| state.pane(pane_idx).selected_indices.iter().min().copied())
        .unwrap_or(0) as usize;
    let anchor_index = anchor_index.min(total.saturating_sub(1));

    let scroll_y = {
        let pane = state.pane(pane_idx);
        let folders = pane.visible_folder_indices();
        let files = pane.visible_file_indices();
        let folders_per_row = items_per_row;
        let folder_rows = folders.len().div_ceil(folders_per_row);
        let file_section_top = folder_rows as f32 * GRID_FOLDER_ROW_ESTIMATED_HEIGHT
            + if !folders.is_empty() && !files.is_empty() {
                GRID_SECTION_SEPARATOR_ESTIMATED_HEIGHT
            } else {
                0.0
            };

        if let Some(folder_position) = folders
            .iter()
            .position(|index| *index as usize == anchor_index)
        {
            let anchor_row = folder_position / folders_per_row;
            anchor_row.saturating_sub(1) as f32 * GRID_FOLDER_ROW_ESTIMATED_HEIGHT
        } else if let Some(file_position) = files
            .iter()
            .position(|index| *index as usize == anchor_index)
        {
            let anchor_row = file_position / items_per_row;
            file_section_top + anchor_row.saturating_sub(1) as f32 * GRID_FILE_ROW_ESTIMATED_HEIGHT
        } else {
            let anchor_row = anchor_index / items_per_row;
            anchor_row.saturating_sub(1) as f32 * GRID_FILE_ROW_ESTIMATED_HEIGHT
        }
    };

    state.pane_mut(pane_idx).scroll_y = scroll_y;
}

pub fn visible_index_range(state: &AppState, pane_idx: usize, scan_end: usize) -> (usize, usize) {
    if scan_end == 0 {
        return (0, 0);
    }

    let pane = state.pane(pane_idx);
    match pane.view_mode {
        ViewMode::List => {
            let viewport_height = if pane.viewport_height > 1.0 {
                pane.viewport_height
            } else {
                (state.window_height - 180.0).max(LIST_ROW_ESTIMATED_HEIGHT)
            };
            let row_scroll = (pane.scroll_y.max(0.0) - LIST_HEADER_ESTIMATED_HEIGHT).max(0.0);
            let start = (row_scroll / LIST_ROW_ESTIMATED_HEIGHT).floor() as usize;
            let visible_rows = (viewport_height / LIST_ROW_ESTIMATED_HEIGHT)
                .ceil()
                .max(1.0) as usize;
            let start = start.min(scan_end.saturating_sub(1));
            let end = (start + visible_rows + 1).min(scan_end);
            (start, end.max(start + 1))
        }
        ViewMode::Grid => visible_grid_index_range(state, pane_idx, scan_end),
    }
}

fn visible_grid_index_range(state: &AppState, pane_idx: usize, scan_end: usize) -> (usize, usize) {
    let pane = state.pane(pane_idx);
    let folder_count = pane.visible_folder_indices().len();
    let file_count = pane.visible_file_indices().len();

    if folder_count + file_count != pane.visible_indices().len() {
        return estimated_flat_grid_index_range(state, pane_idx, scan_end);
    }

    let viewport_height = if pane.viewport_height > 1.0 {
        pane.viewport_height
    } else {
        (state.window_height - 220.0).max(GRID_FILE_ROW_ESTIMATED_HEIGHT)
    };
    let folders_per_row = grid_items_per_row_for_pane(state, pane_idx);
    let folder_rows = folder_count.div_ceil(folders_per_row);
    let file_items_per_row = grid_items_per_row_for_pane(state, pane_idx);
    let file_rows = file_count.div_ceil(file_items_per_row);

    let separator_height = if folder_count > 0 && file_count > 0 {
        GRID_SECTION_SEPARATOR_ESTIMATED_HEIGHT
    } else {
        0.0
    };
    let file_section_top = folder_rows as f32 * GRID_FOLDER_ROW_ESTIMATED_HEIGHT + separator_height;

    let mut start = usize::MAX;
    let mut end = 0usize;

    if let Some((row_start, row_end)) = visible_row_range(
        pane.scroll_y,
        viewport_height,
        folder_rows,
        GRID_FOLDER_ROW_ESTIMATED_HEIGHT,
        0.0,
    ) {
        start = start.min(row_start.saturating_mul(folders_per_row));
        end = end.max(row_end.saturating_mul(folders_per_row).min(folder_count));
    }

    if let Some((row_start, row_end)) = visible_row_range(
        pane.scroll_y,
        viewport_height,
        file_rows,
        GRID_FILE_ROW_ESTIMATED_HEIGHT,
        file_section_top,
    ) {
        let section_start = folder_count + row_start.saturating_mul(file_items_per_row);
        let section_end = folder_count + row_end.saturating_mul(file_items_per_row).min(file_count);
        start = start.min(section_start);
        end = end.max(section_end);
    }

    if start == usize::MAX || end <= start {
        return estimated_flat_grid_index_range(state, pane_idx, scan_end);
    }

    let start = start.min(scan_end.saturating_sub(1));
    let end = end.min(scan_end).max(start + 1);
    (start, end)
}

fn visible_row_range(
    scroll_y: f32,
    viewport_height: f32,
    total_rows: usize,
    row_height: f32,
    section_top: f32,
) -> Option<(usize, usize)> {
    if total_rows == 0 {
        return None;
    }
    let viewport_start = scroll_y.max(0.0);
    let viewport_end = viewport_start + viewport_height;
    let section_end = section_top + total_rows as f32 * row_height;
    if viewport_end < section_top || viewport_start > section_end {
        return None;
    }

    let local_scroll = (viewport_start - section_top).max(0.0);
    let first_visible = (local_scroll / row_height).floor().max(0.0) as usize;
    let visible_rows = (viewport_height / row_height).ceil().max(1.0) as usize + 1;
    let start = first_visible.min(total_rows.saturating_sub(1));
    let end = (first_visible + visible_rows).min(total_rows);
    Some((start, end.max(start + 1)))
}

fn estimated_flat_grid_index_range(
    state: &AppState,
    pane_idx: usize,
    scan_end: usize,
) -> (usize, usize) {
    let items_per_row = grid_items_per_row_for_pane(state, pane_idx);
    let start_row =
        (state.pane(pane_idx).scroll_y.max(0.0) / GRID_FILE_ROW_ESTIMATED_HEIGHT).floor() as usize;
    let viewport_height = if state.pane(pane_idx).viewport_height > 1.0 {
        state.pane(pane_idx).viewport_height
    } else {
        (state.window_height - 220.0).max(GRID_FILE_ROW_ESTIMATED_HEIGHT)
    };
    let visible_rows = (viewport_height / GRID_FILE_ROW_ESTIMATED_HEIGHT)
        .ceil()
        .max(1.0) as usize;
    let start = start_row
        .saturating_mul(items_per_row)
        .min(scan_end.saturating_sub(1));
    let end = (start_row + visible_rows + 1)
        .saturating_mul(items_per_row)
        .min(scan_end);
    (start, end.max(start + 1))
}

pub(super) fn distance_to_visible(index: usize, visible_start: usize, visible_end: usize) -> usize {
    if index < visible_start {
        visible_start - index
    } else if index >= visible_end {
        index - visible_end + 1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core_domain::types::list::ListEntry;

    fn entry(name: &str, is_dir: bool) -> ListEntry {
        crate::test_support::local_entry(&format!("/tmp/{name}"), is_dir)
    }

    #[test]
    fn grid_visible_range_accounts_for_folder_section_height() {
        let mut state = AppState::default();
        state.window_width = 1_000.0;
        state.window_height = 800.0;

        let mut entries = Vec::new();
        for index in 0..24 {
            entries.push(entry(&format!("folder-{index:02}"), true));
        }
        for index in 0..40 {
            entries.push(entry(&format!("image-{index:02}.jpg"), false));
        }

        {
            let pane = state.pane_mut(0);
            pane.view_mode = ViewMode::Grid;
            pane.viewport_height = 300.0;
            pane.replace_entries(entries);
        }

        let folder_count = state.pane(0).visible_folder_indices().len();
        let folders_per_row = grid_items_per_row_for_pane(&state, 0);
        let folder_rows = folder_count.div_ceil(folders_per_row);
        let file_items_per_row = grid_items_per_row_for_pane(&state, 0);
        state.pane_mut(0).scroll_y = folder_rows as f32 * GRID_FOLDER_ROW_ESTIMATED_HEIGHT
            + GRID_SECTION_SEPARATOR_ESTIMATED_HEIGHT
            + GRID_FILE_ROW_ESTIMATED_HEIGHT * 2.0;

        let (start, end) = visible_index_range(&state, 0, state.pane(0).visible_indices().len());

        assert_eq!(start, folder_count + file_items_per_row * 2);
        assert!(end > start);
        assert!(end <= state.pane(0).visible_indices().len());
    }

    #[test]
    fn prepare_grid_view_scrolls_file_anchor_below_folder_section() {
        let mut state = AppState::default();
        state.window_width = 1_000.0;
        let mut entries = Vec::new();
        for index in 0..24 {
            entries.push(entry(&format!("folder-{index:02}"), true));
        }
        for index in 0..40 {
            entries.push(entry(&format!("image-{index:02}.jpg"), false));
        }
        {
            let pane = state.pane_mut(0);
            pane.view_mode = ViewMode::Grid;
            pane.replace_entries(entries);
            pane.focused_index = pane
                .entries
                .iter()
                .position(|entry| entry.name == "image-20.jpg")
                .map(|index| index as EntryIndex);
        }

        let folder_count = state.pane(0).visible_folder_indices().len();
        let folders_per_row = grid_items_per_row_for_pane(&state, 0);
        let folder_rows = folder_count.div_ceil(folders_per_row);
        let file_section_top = folder_rows as f32 * GRID_FOLDER_ROW_ESTIMATED_HEIGHT
            + GRID_SECTION_SEPARATOR_ESTIMATED_HEIGHT;

        prepare_grid_view(&mut state, 0);

        assert!(state.pane(0).scroll_y >= file_section_top);
    }
}
