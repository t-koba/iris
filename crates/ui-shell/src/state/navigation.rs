use super::layout::{ListColumnKind, ListColumnSpec, SortKey, SortOrder, ViewMode};
use chrono::NaiveDate;
use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::object::{ObjectKey, ObjectRef};
use iris_search::filter::{FilterModel, GpsFilter};
use iris_search::segment::IndexedKind;
use iris_search::token::{parse_filter_tokens, ParseWarning};
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

const ENTRY_NOT_VISIBLE: usize = usize::MAX;

pub type EntryIndex = u32;

pub(super) fn entry_offset(index: EntryIndex) -> usize {
    index as usize
}

fn entry_index(offset: usize) -> EntryIndex {
    EntryIndex::try_from(offset).expect("pane entry count exceeds u32 index space")
}

#[derive(Clone, Debug)]
pub struct InlineRenameState {
    pub target: ObjectRef,
    pub original_name: String,
    pub input_value: String,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug)]
pub struct BreadcrumbSegment {
    pub name: String,
    pub full_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathSuggestion {
    pub label: String,
    pub value: String,
}

/// Directory loading is modeled explicitly so an empty result is never
/// confused with a request that has not completed yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PaneLoadState {
    #[default]
    Idle,
    LoadingInitial,
    LoadingMore,
    Loaded,
    Failed {
        message: String,
    },
}

impl PaneLoadState {
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::LoadingInitial | Self::LoadingMore)
    }

    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Failed { message } => Some(message),
            _ => None,
        }
    }
}

pub struct PaneState {
    pub current_path: String,
    pub entries: Vec<ListEntry>,
    pub load_state: PaneLoadState,
    pub navigation_history: Vec<String>,
    pub history_index: usize,
    pub breadcrumb_segments: Vec<BreadcrumbSegment>,
    pub selected_indices: HashSet<EntryIndex>,
    pub selection_anchor_path: Option<String>,
    pub focused_index: Option<EntryIndex>,
    pub(crate) entry_indices_by_path: HashMap<String, EntryIndex>,
    pub(crate) entry_indices_by_object_key: HashMap<ObjectKey, EntryIndex>,
    pub(crate) visible_entry_indices: Vec<EntryIndex>,
    pub(crate) visible_entry_positions: Vec<usize>,
    pub(crate) visible_folder_indices: Vec<EntryIndex>,
    pub(crate) visible_file_indices: Vec<EntryIndex>,
    pub(crate) visible_list_row_offsets: Vec<f32>,
    pub filter_query: String,
    pub path_editing: bool,
    pub path_input: String,
    pub path_suggestions: Vec<PathSuggestion>,
    pub path_suggestion_selected: Option<usize>,
    pub filter_model: FilterModel,
    pub filter_warnings: Vec<ParseWarning>,
    pub filter_date_picker_open: bool,
    pub filter_date_anchor: Option<NaiveDate>,
    pub filter_date_start_input: String,
    pub filter_date_end_input: String,
    pub filter_gps_picker_open: bool,
    pub filter_gps_min_lat_input: String,
    pub filter_gps_max_lat_input: String,
    pub filter_gps_min_lon_input: String,
    pub filter_gps_max_lon_input: String,
    pub filter_gps_points: Vec<(f32, f32)>,
    pub filter_map_tiles:
        BTreeMap<crate::views::filter::map_panel::TileCoord, iced::advanced::image::Handle>,
    pub filter_map_tiles_loading: HashSet<crate::views::filter::map_panel::TileCoord>,
    pub filter_map_tiles_failed: HashMap<crate::views::filter::map_panel::TileCoord, Instant>,
    pub filter_run_id: u64,
    pub filter_recursive_running: bool,
    pub filter_recursive_scanned: usize,
    pub filter_recursive_matched: usize,
    pub filter_cancellation_token: Option<CancellationToken>,
    pub metadata_hydration_inflight: HashSet<String>,
    pub last_click_index: Option<EntryIndex>,
    pub last_click_at: Option<Instant>,
    pub view_mode: ViewMode,
    pub sort_key: SortKey,
    pub sort_order: SortOrder,
    pub scroll_y: f32,
    pub viewport_height: f32,
    pub scroll_velocity_y: f32,
    pub last_scroll_at: Option<Instant>,
    pub list_columns: Vec<ListColumnSpec>,
    pub inline_rename: Option<InlineRenameState>,
    pub(crate) facet_counts: FacetCounts,
}

#[derive(Default)]
pub(crate) struct FacetCounts {
    pub(crate) kind_counts: BTreeMap<IndexedKind, usize>,
    pub(crate) small_files: usize,
    pub(crate) medium_files: usize,
    pub(crate) large_files: usize,
}

impl FacetCounts {
    pub(crate) fn add_values(&mut self, kind: IndexedKind, is_dir: bool, size: Option<u64>) {
        *self.kind_counts.entry(kind).or_insert(0) += 1;
        if !is_dir {
            match size {
                Some(size) if size < 1024 * 1024 => self.small_files += 1,
                Some(size) if size <= 100 * 1024 * 1024 => self.medium_files += 1,
                Some(_) => self.large_files += 1,
                None => {}
            }
        }
    }

    pub(crate) fn remove_values(&mut self, kind: IndexedKind, is_dir: bool, size: Option<u64>) {
        if let Some(count) = self.kind_counts.get_mut(&kind) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.kind_counts.remove(&kind);
            }
        }
        if !is_dir {
            match size {
                Some(size) if size < 1024 * 1024 => {
                    self.small_files = self.small_files.saturating_sub(1)
                }
                Some(size) if size <= 100 * 1024 * 1024 => {
                    self.medium_files = self.medium_files.saturating_sub(1)
                }
                Some(_) => self.large_files = self.large_files.saturating_sub(1),
                None => {}
            }
        }
    }

    fn add_entry(&mut self, entry: &ListEntry) {
        self.add_values(list_entry_kind(entry), entry.is_dir, entry.size);
    }
}

impl Default for PaneState {
    fn default() -> Self {
        Self {
            current_path: String::new(),
            entries: Vec::new(),
            load_state: PaneLoadState::Idle,
            navigation_history: Vec::new(),
            history_index: 0,
            breadcrumb_segments: Vec::new(),
            selected_indices: HashSet::new(),
            selection_anchor_path: None,
            focused_index: None,
            entry_indices_by_path: HashMap::new(),
            entry_indices_by_object_key: HashMap::new(),
            visible_entry_indices: Vec::new(),
            visible_entry_positions: Vec::new(),
            visible_folder_indices: Vec::new(),
            visible_file_indices: Vec::new(),
            visible_list_row_offsets: vec![0.0],
            filter_query: String::new(),
            path_editing: false,
            path_input: String::new(),
            path_suggestions: Vec::new(),
            path_suggestion_selected: None,
            filter_model: FilterModel::default(),
            filter_warnings: Vec::new(),
            filter_date_picker_open: false,
            filter_date_anchor: None,
            filter_date_start_input: String::new(),
            filter_date_end_input: String::new(),
            filter_gps_picker_open: false,
            filter_gps_min_lat_input: String::new(),
            filter_gps_max_lat_input: String::new(),
            filter_gps_min_lon_input: String::new(),
            filter_gps_max_lon_input: String::new(),
            filter_gps_points: Vec::new(),
            filter_map_tiles: BTreeMap::new(),
            filter_map_tiles_loading: HashSet::new(),
            filter_map_tiles_failed: HashMap::new(),
            filter_run_id: 0,
            filter_recursive_running: false,
            filter_recursive_scanned: 0,
            filter_recursive_matched: 0,
            filter_cancellation_token: None,
            metadata_hydration_inflight: HashSet::new(),
            last_click_index: None,
            last_click_at: None,
            view_mode: ViewMode::List,
            sort_key: SortKey::Name,
            sort_order: SortOrder::Ascending,
            scroll_y: 0.0,
            viewport_height: 0.0,
            scroll_velocity_y: 0.0,
            last_scroll_at: None,
            list_columns: ListColumnSpec::default_columns(),
            inline_rename: None,
            facet_counts: FacetCounts::default(),
        }
    }
}

impl PaneState {
    pub fn update_viewport(&mut self, offset_y: f32, viewport_height: f32) -> bool {
        let offset_changed = (self.scroll_y - offset_y).abs() > 2.0;
        let viewport_changed = (self.viewport_height - viewport_height).abs() > 1.0;
        if offset_changed {
            let now = Instant::now();
            let distance = (self.scroll_y - offset_y).abs();
            self.scroll_velocity_y = self.last_scroll_at.map_or(0.0, |last| {
                let elapsed = now.duration_since(last).as_secs_f32().max(0.001);
                let sample = distance / elapsed;
                self.scroll_velocity_y * 0.35 + sample * 0.65
            });
            self.last_scroll_at = Some(now);
        }
        if offset_changed || viewport_changed {
            self.scroll_y = offset_y;
            self.viewport_height = viewport_height;
            true
        } else {
            false
        }
    }

    pub fn rebuild_breadcrumbs(&mut self) {
        let path = PathBuf::from(&self.current_path);
        let mut segments = Vec::new();
        let mut accumulated = PathBuf::new();

        for component in path.components() {
            accumulated.push(component);
            let name = match component {
                std::path::Component::RootDir => "/".to_string(),
                std::path::Component::Normal(segment) => segment.to_string_lossy().to_string(),
                _ => continue,
            };
            segments.push(BreadcrumbSegment {
                name,
                full_path: accumulated.to_string_lossy().to_string(),
            });
        }

        self.breadcrumb_segments = segments;
    }

    pub fn push_to_history(&mut self, path: String) {
        if self.history_index < self.navigation_history.len() {
            self.navigation_history.truncate(self.history_index);
        }
        self.navigation_history.push(path);
        self.history_index = self.navigation_history.len();
    }

    pub fn can_go_back(&self) -> bool {
        self.history_index > 1
    }

    pub fn can_go_forward(&self) -> bool {
        self.history_index < self.navigation_history.len()
    }

    pub fn go_back(&mut self) -> Option<String> {
        if self.can_go_back() {
            self.history_index -= 1;
            self.navigation_history.get(self.history_index - 1).cloned()
        } else {
            None
        }
    }

    pub fn go_forward(&mut self) -> Option<String> {
        if self.can_go_forward() {
            let path = self.navigation_history.get(self.history_index).cloned();
            self.history_index += 1;
            path
        } else {
            None
        }
    }

    pub fn visible_list_columns(&self, compact: bool) -> Vec<&ListColumnSpec> {
        self.list_columns
            .iter()
            .filter(|column| column.visible && (!compact || column.kind == ListColumnKind::Name))
            .collect()
    }

    pub fn list_column_width(&self, kind: ListColumnKind) -> f32 {
        self.list_columns
            .iter()
            .find(|column| column.kind == kind)
            .map(|column| column.width)
            .unwrap_or_else(|| {
                ListColumnSpec::default_columns()
                    .into_iter()
                    .find(|column| column.kind == kind)
                    .map(|column| column.width)
                    .unwrap_or(80.0)
            })
    }

    pub fn set_list_column_width(&mut self, kind: ListColumnKind, width: f32) {
        if let Some(column) = self
            .list_columns
            .iter_mut()
            .find(|column| column.kind == kind)
        {
            column.width = width;
        }
    }

    pub fn sort_entries(&mut self) {
        self.rebuild_visible_indices();
    }

    pub fn replace_entries(&mut self, entries: Vec<ListEntry>) {
        self.inline_rename = None;
        self.selected_indices.clear();
        self.selection_anchor_path = None;
        self.focused_index = None;
        self.entries = entries;
        self.facet_counts = FacetCounts::default();
        for entry in &self.entries {
            self.facet_counts.add_entry(entry);
        }
        self.rebuild_entry_indices_by_path();
        self.rebuild_visible_indices();
    }

    pub fn clear_entries(&mut self) {
        self.entries.clear();
        self.entry_indices_by_path.clear();
        self.entry_indices_by_object_key.clear();
        self.visible_entry_indices.clear();
        self.visible_entry_positions.clear();
        self.visible_folder_indices.clear();
        self.visible_file_indices.clear();
        self.visible_list_row_offsets.clear();
        self.visible_list_row_offsets.push(0.0);
        self.inline_rename = None;
        self.facet_counts = FacetCounts::default();
    }

    pub fn append_entries(&mut self, entries: Vec<ListEntry>) {
        if entries.is_empty() {
            return;
        }

        for entry in &entries {
            self.facet_counts.add_entry(entry);
        }
        let first_new_index = self.entries.len();
        self.entries.extend(entries);
        self.index_entry_paths_from(first_new_index);
        self.visible_entry_positions
            .resize(self.entries.len(), ENTRY_NOT_VISIBLE);

        let mut new_visible_indices: Vec<EntryIndex> = if self.filter_query.trim().is_empty() {
            (first_new_index..self.entries.len())
                .map(entry_index)
                .collect()
        } else {
            (first_new_index..self.entries.len())
                .filter(|index| self.entry_matches_filter(*index))
                .map(entry_index)
                .collect()
        };
        if new_visible_indices.is_empty() {
            return;
        }

        sort_visible_indices_by(
            &self.entries,
            &mut new_visible_indices,
            self.sort_key,
            self.sort_order,
        );
        merge_visible_indices_by(
            &self.entries,
            &mut self.visible_entry_indices,
            new_visible_indices,
            self.sort_key,
            self.sort_order,
        );
        self.rebuild_visible_entry_positions();
        self.rebuild_visible_grid_sections();
        self.rebuild_list_row_offsets();
    }

    pub fn is_filter_active(&self) -> bool {
        !self.filter_query.trim().is_empty()
    }

    pub fn next_filter_run_id(&mut self) -> u64 {
        self.filter_run_id = self.filter_run_id.wrapping_add(1);
        self.filter_run_id
    }

    pub fn begin_recursive_filter(&mut self) -> (u64, CancellationToken) {
        self.cancel_filter_task();
        let run_id = self.next_filter_run_id();
        let cancellation_token = CancellationToken::new();
        self.clear_entries();
        self.metadata_hydration_inflight.clear();
        self.selected_indices.clear();
        self.selection_anchor_path = None;
        self.focused_index = None;
        self.load_state = PaneLoadState::LoadingInitial;
        self.filter_recursive_running = true;
        self.filter_recursive_scanned = 0;
        self.filter_recursive_matched = 0;
        self.filter_gps_points.clear();
        self.filter_map_tiles.clear();
        self.filter_map_tiles_loading.clear();
        self.filter_map_tiles_failed.clear();
        self.filter_cancellation_token = Some(cancellation_token.clone());
        (run_id, cancellation_token)
    }

    pub fn stop_recursive_filter(&mut self) {
        self.cancel_filter_task();
        self.next_filter_run_id();
        if self.filter_recursive_running {
            self.load_state = PaneLoadState::Loaded;
        }
        self.filter_recursive_running = false;
        self.filter_recursive_scanned = 0;
        self.filter_recursive_matched = 0;
        self.filter_gps_points.clear();
        self.filter_map_tiles.clear();
        self.filter_map_tiles_loading.clear();
        self.filter_map_tiles_failed.clear();
    }

    fn cancel_filter_task(&mut self) {
        if let Some(token) = self.filter_cancellation_token.take() {
            token.cancel();
        }
    }

    pub fn entry_matches_filter(&self, index: usize) -> bool {
        let Some(entry) = self.entries.get(index) else {
            return false;
        };
        entry_matches_model(entry, &self.filter_model)
    }

    pub fn visible_entries(&self) -> impl Iterator<Item = (EntryIndex, &ListEntry)> {
        self.visible_entry_indices.iter().filter_map(|index| {
            self.entries
                .get(entry_offset(*index))
                .map(|entry| (*index, entry))
        })
    }

    pub fn visible_indices(&self) -> &[EntryIndex] {
        &self.visible_entry_indices
    }

    pub fn entry(&self, index: EntryIndex) -> Option<&ListEntry> {
        self.entries.get(entry_offset(index))
    }

    pub fn inline_rename_for(&self, index: EntryIndex) -> Option<&InlineRenameState> {
        let rename = self.inline_rename.as_ref()?;
        let entry = self.entries.get(entry_offset(index))?;
        (entry.object_ref.key == rename.target.key).then_some(rename)
    }

    pub fn begin_inline_rename(&mut self, index: EntryIndex) -> Option<ObjectRef> {
        let entry = self.entries.get(entry_offset(index))?.clone();
        self.select_single(index);
        self.inline_rename = Some(InlineRenameState {
            target: entry.object_ref.clone(),
            original_name: entry.name.clone(),
            input_value: entry.name,
            error_message: None,
        });
        Some(entry.object_ref)
    }

    pub fn update_inline_rename_input(&mut self, index: EntryIndex, value: String) {
        let Some(entry) = self.entries.get(entry_offset(index)) else {
            self.inline_rename = None;
            return;
        };
        match &mut self.inline_rename {
            Some(rename) if rename.target.key == entry.object_ref.key => {
                rename.input_value = value;
                rename.error_message = None;
            }
            _ => {}
        }
        self.rebuild_list_row_offsets();
    }

    pub fn cancel_inline_rename(&mut self) {
        self.inline_rename = None;
    }

    pub fn take_inline_rename_for(&mut self, index: EntryIndex) -> Option<InlineRenameState> {
        let entry_key = self
            .entries
            .get(entry_offset(index))?
            .object_ref
            .key
            .clone();
        if self
            .inline_rename
            .as_ref()
            .is_some_and(|rename| rename.target.key == entry_key)
        {
            self.inline_rename.take()
        } else {
            None
        }
    }

    pub fn visible_folder_indices(&self) -> &[EntryIndex] {
        &self.visible_folder_indices
    }

    pub fn visible_file_indices(&self) -> &[EntryIndex] {
        &self.visible_file_indices
    }

    pub fn first_visible_index(&self) -> Option<EntryIndex> {
        self.visible_entry_indices.first().copied()
    }

    pub fn last_visible_index(&self) -> Option<EntryIndex> {
        self.visible_entry_indices.last().copied()
    }

    pub fn visible_position(&self, index: EntryIndex) -> Option<usize> {
        self.visible_entry_positions
            .get(entry_offset(index))
            .copied()
            .filter(|position| *position != ENTRY_NOT_VISIBLE)
    }

    pub(crate) fn entry_index_by_display_path(&self, path: &str) -> Option<EntryIndex> {
        if let Some(index) = self.entry_indices_by_path.get(path).copied() {
            if self
                .entries
                .get(entry_offset(index))
                .is_some_and(|entry| entry.object_ref.display_path == path)
            {
                return Some(index);
            }
        }

        self.entries
            .iter()
            .position(|entry| entry.object_ref.display_path == path)
            .map(entry_index)
    }

    pub(crate) fn entry_by_display_path(&self, path: &str) -> Option<&ListEntry> {
        self.entry_index_by_display_path(path)
            .and_then(|index| self.entries.get(entry_offset(index)))
    }

    pub(crate) fn entry_index_by_object_key(&self, object_key: &ObjectKey) -> Option<EntryIndex> {
        if let Some(index) = self.entry_indices_by_object_key.get(object_key).copied() {
            if self
                .entries
                .get(entry_offset(index))
                .is_some_and(|entry| entry.object_ref.key == *object_key)
            {
                return Some(index);
            }
        }

        self.entries
            .iter()
            .position(|entry| entry.object_ref.key == *object_key)
            .map(entry_index)
    }

    pub(crate) fn entry_mut_by_display_path(&mut self, path: &str) -> Option<&mut ListEntry> {
        let indexed = self
            .entry_indices_by_path
            .get(path)
            .copied()
            .filter(|index| {
                self.entries
                    .get(entry_offset(*index))
                    .is_some_and(|entry| entry.object_ref.display_path == path)
            });
        let index = match indexed {
            Some(index) => index,
            None => {
                let index = self
                    .entries
                    .iter()
                    .position(|entry| entry.object_ref.display_path == path)?;
                let index = entry_index(index);
                self.entry_indices_by_path.insert(path.to_string(), index);
                index
            }
        };
        self.entries.get_mut(entry_offset(index))
    }

    pub fn set_filter_query(&mut self, query: String) {
        let parsed = parse_filter_tokens(&query);
        self.filter_query = query;
        self.filter_model = parsed.model;
        self.filter_warnings = parsed.warnings;
        self.rebuild_visible_indices();
    }

    pub fn rebuild_visible_indices(&mut self) {
        let mut indices: Vec<EntryIndex> = if self.filter_query.trim().is_empty() {
            (0..self.entries.len()).map(entry_index).collect()
        } else {
            self.entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    entry_matches_model(entry, &self.filter_model).then_some(entry_index(index))
                })
                .collect()
        };
        sort_visible_indices_by(&self.entries, &mut indices, self.sort_key, self.sort_order);
        self.visible_entry_indices = indices;
        self.rebuild_visible_entry_positions();
        self.rebuild_visible_grid_sections();
        self.rebuild_list_row_offsets();

        let visible_entry_positions = &self.visible_entry_positions;
        self.selected_indices.retain(|index| {
            visible_entry_positions
                .get(entry_offset(*index))
                .is_some_and(|position| *position != ENTRY_NOT_VISIBLE)
        });
        if self
            .focused_index
            .is_some_and(|index| self.visible_position(index).is_none())
        {
            self.focused_index = self.first_visible_index();
        }
        if self.selection_anchor_index().is_none() {
            self.selection_anchor_path = self
                .focused_index
                .and_then(|index| self.entry_display_path(index));
        }
        if self.inline_rename.as_ref().is_some_and(|rename| {
            self.entry_index_by_object_key(&rename.target.key)
                .and_then(|index| self.visible_position(index))
                .is_none()
        }) {
            self.inline_rename = None;
        }
    }

    fn rebuild_visible_grid_sections(&mut self) {
        self.visible_folder_indices.clear();
        self.visible_file_indices.clear();
        self.visible_folder_indices
            .reserve(self.visible_entry_indices.len());
        self.visible_file_indices
            .reserve(self.visible_entry_indices.len());
        for index in &self.visible_entry_indices {
            match self.entries.get(entry_offset(*index)) {
                Some(entry) if entry.is_dir => self.visible_folder_indices.push(*index),
                Some(_) => self.visible_file_indices.push(*index),
                None => {}
            }
        }
    }

    pub(crate) fn rebuild_list_row_offsets(&mut self) {
        const BASE_HEIGHT: f32 = crate::widgets::metrics::LIST_ROW_HEIGHT;
        const DETAIL_HEIGHT: f32 = 14.0;
        self.visible_list_row_offsets.clear();
        self.visible_list_row_offsets
            .reserve(self.visible_entry_indices.len() + 1);
        self.visible_list_row_offsets.push(0.0);
        let recursive = self.filter_model.scope == iris_search::filter::Scope::Recursive;
        let rename = self.inline_rename.as_ref();
        for index in &self.visible_entry_indices {
            let Some(entry) = self.entries.get(entry_offset(*index)) else {
                continue;
            };
            let mut height = BASE_HEIGHT;
            if recursive {
                height += DETAIL_HEIGHT;
            }
            if entry
                .annotation
                .as_deref()
                .is_some_and(|value| !value.is_empty())
            {
                height += DETAIL_HEIGHT;
            }
            if rename.is_some_and(|rename| {
                rename.target.key == entry.object_ref.key && rename.error_message.is_some()
            }) {
                height += DETAIL_HEIGHT;
            }
            let next = self.visible_list_row_offsets.last().copied().unwrap_or(0.0) + height;
            self.visible_list_row_offsets.push(next);
        }
    }

    pub(crate) fn list_row_offset(&self, position: usize) -> f32 {
        self.visible_list_row_offsets
            .get(position)
            .copied()
            .unwrap_or_else(|| self.list_rows_height())
    }

    pub(crate) fn list_rows_height(&self) -> f32 {
        self.visible_list_row_offsets.last().copied().unwrap_or(0.0)
    }

    fn rebuild_visible_entry_positions(&mut self) {
        self.visible_entry_positions.clear();
        self.visible_entry_positions
            .resize(self.entries.len(), ENTRY_NOT_VISIBLE);
        for (position, index) in self.visible_entry_indices.iter().copied().enumerate() {
            if let Some(slot) = self.visible_entry_positions.get_mut(entry_offset(index)) {
                *slot = position;
            }
        }
    }

    fn rebuild_entry_indices_by_path(&mut self) {
        self.entry_indices_by_path.clear();
        self.entry_indices_by_path.reserve(self.entries.len());
        self.entry_indices_by_object_key.clear();
        self.entry_indices_by_object_key.reserve(self.entries.len());
        self.index_entry_paths_from(0);
    }

    fn index_entry_paths_from(&mut self, start: usize) {
        for (offset, entry) in self.entries[start..].iter().enumerate() {
            let index = start + offset;
            self.entry_indices_by_path
                .entry(entry.object_ref.display_path.clone())
                .or_insert(entry_index(index));
            self.entry_indices_by_object_key
                .entry(entry.object_ref.key.clone())
                .or_insert(entry_index(index));
        }
    }
}

fn sort_visible_indices_by(
    entries: &[ListEntry],
    indices: &mut [EntryIndex],
    sort_key: SortKey,
    sort_order: SortOrder,
) {
    match (sort_key, sort_order) {
        (SortKey::Name, SortOrder::Ascending) => {
            indices
                .sort_by(|left, right| compare_entries(entries, *left, *right, compare_name_asc));
        }
        (SortKey::Name, SortOrder::Descending) => {
            indices
                .sort_by(|left, right| compare_entries(entries, *left, *right, compare_name_desc));
        }
        (SortKey::Date | SortKey::ModifiedDate, SortOrder::Ascending) => {
            indices.sort_by_cached_key(|index| {
                entries
                    .get(entry_offset(*index))
                    .map(|entry| (file_group_key(entry), entry.mtime, *index))
            });
        }
        (SortKey::Date | SortKey::ModifiedDate, SortOrder::Descending) => {
            indices.sort_by_cached_key(|index| {
                entries
                    .get(entry_offset(*index))
                    .map(|entry| (file_group_key(entry), Reverse(entry.mtime), *index))
            });
        }
        (SortKey::CreatedDate, SortOrder::Ascending) => {
            indices.sort_by_cached_key(|index| {
                entries
                    .get(entry_offset(*index))
                    .map(|entry| (file_group_key(entry), entry.ctime, *index))
            });
        }
        (SortKey::CreatedDate, SortOrder::Descending) => {
            indices.sort_by_cached_key(|index| {
                entries
                    .get(entry_offset(*index))
                    .map(|entry| (file_group_key(entry), Reverse(entry.ctime), *index))
            });
        }
        (SortKey::Size, SortOrder::Ascending) => {
            indices.sort_by_cached_key(|index| {
                entries
                    .get(entry_offset(*index))
                    .map(|entry| (file_group_key(entry), entry.size, *index))
            });
        }
        (SortKey::Size, SortOrder::Descending) => {
            indices.sort_by_cached_key(|index| {
                entries
                    .get(entry_offset(*index))
                    .map(|entry| (file_group_key(entry), Reverse(entry.size), *index))
            });
        }
        (SortKey::FileType, SortOrder::Ascending) => {
            indices
                .sort_by(|left, right| compare_entries(entries, *left, *right, compare_type_asc));
        }
        (SortKey::FileType, SortOrder::Descending) => {
            indices
                .sort_by(|left, right| compare_entries(entries, *left, *right, compare_type_desc));
        }
    }
}

fn merge_visible_indices_by(
    entries: &[ListEntry],
    visible_indices: &mut Vec<EntryIndex>,
    incoming_indices: Vec<EntryIndex>,
    sort_key: SortKey,
    sort_order: SortOrder,
) {
    if visible_indices.is_empty() {
        *visible_indices = incoming_indices;
        return;
    }
    if incoming_indices.is_empty() {
        return;
    }

    let existing_indices = std::mem::take(visible_indices);
    let mut merged = Vec::with_capacity(existing_indices.len() + incoming_indices.len());
    let mut existing_position = 0;
    let mut incoming_position = 0;

    while existing_position < existing_indices.len() && incoming_position < incoming_indices.len() {
        let existing_index = existing_indices[existing_position];
        let incoming_index = incoming_indices[incoming_position];
        if compare_visible_indices(
            entries,
            existing_index,
            incoming_index,
            sort_key,
            sort_order,
        ) != Ordering::Greater
        {
            merged.push(existing_index);
            existing_position += 1;
        } else {
            merged.push(incoming_index);
            incoming_position += 1;
        }
    }

    merged.extend_from_slice(&existing_indices[existing_position..]);
    merged.extend_from_slice(&incoming_indices[incoming_position..]);
    *visible_indices = merged;
}

fn compare_visible_indices(
    entries: &[ListEntry],
    left_index: EntryIndex,
    right_index: EntryIndex,
    sort_key: SortKey,
    sort_order: SortOrder,
) -> Ordering {
    match (
        entries.get(entry_offset(left_index)),
        entries.get(entry_offset(right_index)),
    ) {
        (Some(left), Some(right)) => compare_entry_order(left, right, sort_key, sort_order)
            .then_with(|| left_index.cmp(&right_index)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => left_index.cmp(&right_index),
    }
}

fn compare_entry_order(
    left: &ListEntry,
    right: &ListEntry,
    sort_key: SortKey,
    sort_order: SortOrder,
) -> Ordering {
    match (sort_key, sort_order) {
        (SortKey::Name, SortOrder::Ascending) => compare_name_asc(left, right),
        (SortKey::Name, SortOrder::Descending) => compare_name_desc(left, right),
        (SortKey::Date | SortKey::ModifiedDate, SortOrder::Ascending) => {
            compare_grouped_entries(left, right).then_with(|| left.mtime.cmp(&right.mtime))
        }
        (SortKey::Date | SortKey::ModifiedDate, SortOrder::Descending) => {
            compare_grouped_entries(left, right).then_with(|| right.mtime.cmp(&left.mtime))
        }
        (SortKey::CreatedDate, SortOrder::Ascending) => {
            compare_grouped_entries(left, right).then_with(|| left.ctime.cmp(&right.ctime))
        }
        (SortKey::CreatedDate, SortOrder::Descending) => {
            compare_grouped_entries(left, right).then_with(|| right.ctime.cmp(&left.ctime))
        }
        (SortKey::Size, SortOrder::Ascending) => {
            compare_grouped_entries(left, right).then_with(|| left.size.cmp(&right.size))
        }
        (SortKey::Size, SortOrder::Descending) => {
            compare_grouped_entries(left, right).then_with(|| right.size.cmp(&left.size))
        }
        (SortKey::FileType, SortOrder::Ascending) => compare_type_asc(left, right),
        (SortKey::FileType, SortOrder::Descending) => compare_type_desc(left, right),
    }
}

fn compare_entries(
    entries: &[ListEntry],
    left_index: EntryIndex,
    right_index: EntryIndex,
    compare: fn(&ListEntry, &ListEntry) -> Ordering,
) -> Ordering {
    match (
        entries.get(entry_offset(left_index)),
        entries.get(entry_offset(right_index)),
    ) {
        (Some(left), Some(right)) => compare(left, right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => left_index.cmp(&right_index),
    }
}

fn compare_name_asc(left: &ListEntry, right: &ListEntry) -> Ordering {
    compare_grouped_entries(left, right)
        .then_with(|| left.sort_name_key.cmp(&right.sort_name_key))
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| {
            left.object_ref
                .display_path
                .cmp(&right.object_ref.display_path)
        })
}

fn compare_name_desc(left: &ListEntry, right: &ListEntry) -> Ordering {
    compare_grouped_entries(left, right)
        .then_with(|| right.sort_name_key.cmp(&left.sort_name_key))
        .then_with(|| right.name.cmp(&left.name))
        .then_with(|| {
            right
                .object_ref
                .display_path
                .cmp(&left.object_ref.display_path)
        })
}

fn compare_type_asc(left: &ListEntry, right: &ListEntry) -> Ordering {
    compare_grouped_entries(left, right)
        .then_with(|| left.sort_extension_key.cmp(&right.sort_extension_key))
        .then_with(|| left.sort_name_key.cmp(&right.sort_name_key))
        .then_with(|| {
            left.object_ref
                .display_path
                .cmp(&right.object_ref.display_path)
        })
}

fn compare_type_desc(left: &ListEntry, right: &ListEntry) -> Ordering {
    compare_grouped_entries(left, right)
        .then_with(|| right.sort_extension_key.cmp(&left.sort_extension_key))
        .then_with(|| right.sort_name_key.cmp(&left.sort_name_key))
        .then_with(|| {
            right
                .object_ref
                .display_path
                .cmp(&left.object_ref.display_path)
        })
}

fn compare_grouped_entries(left: &ListEntry, right: &ListEntry) -> Ordering {
    file_group_key(left).cmp(&file_group_key(right))
}

fn file_group_key(entry: &ListEntry) -> u8 {
    u8::from(!entry.is_dir)
}

fn entry_matches_model(entry: &ListEntry, model: &FilterModel) -> bool {
    if !model.text.trim().is_empty()
        && iris_search::filter::fuzzy_name_score(&entry.name, &model.text).is_none()
    {
        return false;
    }
    if !model.kinds.is_empty() && !model.kinds.contains(&list_entry_kind(entry)) {
        return false;
    }
    if let Some(range) = &model.size {
        let Some(size) = entry.size else {
            return false;
        };
        if !range.contains(size) {
            return false;
        }
    }
    if let Some(range) = &model.date {
        let Some(mtime) = entry.mtime.map(system_time_to_secs) else {
            return false;
        };
        if !range.contains(mtime) {
            return false;
        }
    }
    match model.gps {
        GpsFilter::Any | GpsFilter::Absent => {}
        GpsFilter::Present | GpsFilter::Bounds(_) => return false,
    }
    model.camera.is_none() && model.lens.is_none()
}

pub(crate) fn list_entry_kind(entry: &ListEntry) -> IndexedKind {
    if entry.is_dir {
        return IndexedKind::Directory;
    }
    let Some(extension) = std::path::Path::new(&entry.name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
    else {
        return IndexedKind::File;
    };
    match extension.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "hif" | "tif" | "tiff"
        | "bmp" | "avif" | "jp2" | "j2k" | "j2c" | "jpc" | "jpf" | "jpx" | "jpm" | "jxr"
        | "hdp" | "wdp" | "jxl" | "arw" | "cr2" | "cr3" | "nef" | "nrw" | "orf" | "rw2" | "raf"
        | "dng" | "x3f" => IndexedKind::Image,
        "mp4" | "mov" | "m4v" | "mkv" | "avi" | "webm" | "mpg" | "mpeg" | "mj2" => {
            IndexedKind::Video
        }
        "pdf" => IndexedKind::Pdf,
        _ => IndexedKind::File,
    }
}

fn system_time_to_secs(value: SystemTime) -> i64 {
    match value.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs().min(i64::MAX as u64) as i64,
        Err(error) => -(error.duration().as_secs().min(i64::MAX as u64) as i64),
    }
}
