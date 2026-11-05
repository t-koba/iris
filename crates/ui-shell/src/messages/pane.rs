use crate::state::{EntryIndex, SortKey, ViewMode};
use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::metadata::MetadataResult;
use iris_core_domain::types::object::ObjectRef;

#[derive(Debug, Clone)]
pub struct EntryMetadataUpdate {
    pub object_ref: ObjectRef,
    pub result: Result<MetadataResult, String>,
}

/// Per-pane messages, sent as `Message::Pane(pane_idx, msg)`.
#[derive(Debug, Clone)]
pub enum PaneMessage {
    NavigateTo(EntryIndex),
    NavigateToPath(String),
    NavigateToBreadcrumb(usize),
    DirectoryLoaded {
        path: String,
        entries: Vec<ListEntry>,
    },
    /// First batch of a progressive load.
    DirectoryHeadLoaded {
        path: String,
        entries: Vec<ListEntry>,
    },
    /// Following batches of a progressive load.
    DirectoryBatchLoaded {
        path: String,
        entries: Vec<ListEntry>,
        done: bool,
    },
    /// Lazy metadata for the visible range.
    MetadataHydrated(Vec<EntryMetadataUpdate>),
    DirectoryLoadFailed {
        path: String,
        error: String,
    },
    SelectItem(EntryIndex),
    ToggleSelection(EntryIndex),
    SelectAll,
    ClearSelection,
    FocusNext,
    FocusPrevious,
    /// Grid view.
    FocusNextRow,
    /// Grid view.
    FocusPreviousRow,
    ActivateFocused,
    SetViewMode(ViewMode),
    SetSortKey(SortKey),
    ToggleSortOrder,
    SetFilter(String),
    PathInputChanged(String),
    CommitPathEdit,
    CancelPathEdit,
    PathSuggestionNext,
    PathSuggestionPrevious,
    /// Copies the selected (or first) suggestion into the input.
    AcceptPathSuggestion,
    CommitPathSuggestion(usize),
    /// Appends a structured token to the current filter.
    AppendFilterToken(String),
    ToggleDateRangePicker,
    SetDateRangeStart(String),
    SetDateRangeEnd(String),
    ApplyDateRange,
    ClearDateRange,
    SelectDateRangeDay(String),
    PreviousDateRangeMonth,
    NextDateRangeMonth,
    PresetDateToday,
    PresetDateMonth,
    PresetDateYear,
    ToggleGpsBoundsPicker,
    SetGpsMinLat(String),
    SetGpsMaxLat(String),
    SetGpsMinLon(String),
    SetGpsMaxLon(String),
    ApplyGpsBounds,
    /// Rectangle selected on the GPS map.
    ApplyGpsBoundsFromMap {
        min_lat: f32,
        max_lat: f32,
        min_lon: f32,
        max_lon: f32,
    },
    GpsMapVisibleTiles(Vec<crate::views::filter::map_panel::TileCoord>),
    GpsMapTileLoaded {
        tile: crate::views::filter::map_panel::TileCoord,
        result: Result<Vec<u8>, String>,
    },
    ClearGpsBounds,
    RecursiveFilterBatch {
        run_id: u64,
        entries: Vec<ListEntry>,
        scanned: usize,
        matched: usize,
        done: bool,
    },
    RecursiveFilterProgress {
        run_id: u64,
        scanned: usize,
        matched: usize,
    },
    /// GPS points within the recursive filter scope.
    RecursiveFilterGpsPoints {
        run_id: u64,
        points: Vec<(f32, f32)>,
    },
    RecursiveFilterFailed {
        run_id: u64,
        error: String,
    },
    CancelRecursiveFilter,
    LoadMoreItems,
    Scrolled {
        offset_y: f32,
        viewport_height: f32,
    },
    GridScrolled {
        offset_y: f32,
        viewport_height: f32,
    },
    InlineRenameInputChanged {
        index: EntryIndex,
        value: String,
    },
    CommitInlineRename(EntryIndex),
    CancelInlineRename,
}
