use crate::messages::{Message, ThumbnailData};
use crate::runtime::app_runtime;
use crate::state::{
    AppState, CachedThumbnail, EntryCacheStamp, EntryIndex, ThumbnailCacheKey, ViewMode,
};
use crate::update::helpers::grid_items_per_row_for_pane;
use iced::Task;
use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::object::ObjectKey;
use iris_core_domain::types::thumbnail::{ImageFormat, ThumbnailHint};
use iris_thumb_preview::scheduler::{RequestPriority, ThumbnailRequest, ThumbnailScheduler};
use std::path::Path;

mod cache;
mod candidates;
mod queue;
mod support;
mod viewport;

pub use cache::{
    clear_thumbnail_cache, insert_thumbnail_cache, invalidate_changed_thumbnail_entries,
    prune_thumbnail_cache, stop_thumbnail_work, thumbnail_cache_key_is_current,
    touch_thumbnail_cache_entry, trim_thumbnail_cache_to_limits,
    trim_thumbnail_cache_to_memory_budget,
};
pub use candidates::is_thumbnail_candidate;
pub use queue::{
    generate_thumbnails, seed_thumbnail_generation_queue, sync_thumbnail_generation_queue,
};
pub use viewport::{prepare_grid_view, visible_index_range};

const THUMB_SIZE: u32 = 128;
pub const MAX_THUMBNAIL_CACHE_ITEMS: usize = 400;
pub(super) const LIST_ROW_ESTIMATED_HEIGHT: f32 = 34.0;
