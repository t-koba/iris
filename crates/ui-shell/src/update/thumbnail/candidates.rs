use super::support::{
    is_background_media_path, is_lightweight_image_path, is_video_path, thumbnail_priority_rank,
};
use super::viewport::{distance_to_visible, visible_index_range};
use super::*;

const FAST_SCROLL_VELOCITY_PX_PER_SEC: f32 = 2_400.0;
const FAST_SCROLL_GATE_WINDOW: std::time::Duration = std::time::Duration::from_millis(200);

pub fn is_thumbnail_candidate(name: &str) -> bool {
    let lower = name.to_lowercase();
    let ext = Path::new(&lower)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();

    is_lightweight_image_path(&lower)
        || matches!(
            ext,
            "heic"
                | "heif"
                | "hif"
                | "avif"
                | "jp2"
                | "j2k"
                | "j2c"
                | "jpc"
                | "jpf"
                | "jpx"
                | "jpm"
                | "jxr"
                | "hdp"
                | "wdp"
                | "jxl"
        )
        || (ext == "pdf" && iris_thumb_preview::pdf::supports_pdf_thumbnails())
        || iris_thumb_preview::raw::supports_raw_extension(ext)
        || (is_video_path(&lower) && iris_thumb_preview::video::supports_video_thumbnails())
}

pub(super) fn request_priority_for(distance: usize, path: &str) -> RequestPriority {
    if distance == 0 {
        RequestPriority::Visible
    } else if is_background_thumbnail(path) {
        RequestPriority::Background
    } else {
        RequestPriority::Nearby
    }
}

fn pane_is_scrolling_fast(state: &AppState, pane_idx: usize) -> bool {
    let pane = state.pane(pane_idx);
    pane.scroll_velocity_y >= FAST_SCROLL_VELOCITY_PX_PER_SEC
        && pane
            .last_scroll_at
            .is_some_and(|last| last.elapsed() <= FAST_SCROLL_GATE_WINDOW)
}

fn request_priority_rank(priority: RequestPriority) -> u8 {
    match priority {
        RequestPriority::Visible => 0,
        RequestPriority::Nearby => 1,
        RequestPriority::Background => 2,
    }
}

/// Deduplicated by cache key, keeping the best priority.
pub(super) fn collect_missing_candidates(
    state: &AppState,
) -> Vec<(usize, u8, RequestPriority, EntryIndex, usize)> {
    let mut best = std::collections::HashMap::<
        ThumbnailCacheKey,
        (usize, u8, RequestPriority, EntryIndex, usize),
    >::new();

    for pane_idx in 0..2 {
        if state.pane(pane_idx).view_mode != ViewMode::Grid {
            continue;
        }
        let visible_indices = state.pane(pane_idx).visible_indices();
        if visible_indices.is_empty() {
            continue;
        }

        let (visible_start, visible_end) =
            visible_index_range(state, pane_idx, visible_indices.len());
        let (prefetch_start, prefetch_end) =
            thumbnail_prefetch_range(state, pane_idx, visible_indices.len());
        let scrolling_fast = pane_is_scrolling_fast(state, pane_idx);

        for visible_position in prefetch_start..prefetch_end {
            let Some(index) = visible_indices.get(visible_position).copied() else {
                continue;
            };
            let Some(entry) = state.pane(pane_idx).entry(index) else {
                continue;
            };
            if entry.is_dir || !is_thumbnail_candidate(&entry.name) {
                continue;
            }

            let key = ThumbnailCacheKey::from_entry(entry);
            if state.thumbnail_cache.contains_key(&key)
                || state.thumbnail_inflight.contains(&key)
                || state.thumbnail_unsupported.contains(&key)
            {
                continue;
            }
            let distance = distance_to_visible(visible_position, visible_start, visible_end);
            if scrolling_fast && distance > 0 {
                continue;
            }
            let full_path = key.path();
            if !should_queue_thumbnail_for_path(
                full_path,
                state.pane(pane_idx).view_mode.clone(),
                distance,
            ) {
                continue;
            }
            let media_priority = thumbnail_priority(full_path);
            let request_priority = request_priority_for(distance, full_path);

            let candidate = (distance, media_priority, request_priority, index, pane_idx);
            best.entry(key)
                .and_modify(|existing| {
                    let existing_rank = (request_priority_rank(existing.2), existing.0, existing.1);
                    let new_rank = (
                        request_priority_rank(request_priority),
                        distance,
                        media_priority,
                    );
                    if new_rank < existing_rank {
                        *existing = candidate;
                    }
                })
                .or_insert(candidate);
        }
    }

    let mut result: Vec<_> = best.into_values().collect();
    result.sort_by_key(|(distance, media_priority, priority, _, _)| {
        (request_priority_rank(*priority), *distance, *media_priority)
    });
    result
}

/// Paths inside every pane's prefetch range.
pub(super) fn collect_desired_paths(
    state: &AppState,
) -> std::collections::HashSet<ThumbnailCacheKey> {
    let mut desired = std::collections::HashSet::new();
    for pane_idx in 0..2 {
        if state.pane(pane_idx).view_mode != ViewMode::Grid {
            continue;
        }
        let visible_indices = state.pane(pane_idx).visible_indices();
        if visible_indices.is_empty() {
            continue;
        }
        let (visible_start, visible_end) =
            visible_index_range(state, pane_idx, visible_indices.len());
        let (prefetch_start, prefetch_end) =
            thumbnail_prefetch_range(state, pane_idx, visible_indices.len());
        let scrolling_fast = pane_is_scrolling_fast(state, pane_idx);
        for visible_position in prefetch_start..prefetch_end {
            let Some(index) = visible_indices.get(visible_position).copied() else {
                continue;
            };
            let Some(entry) = state.pane(pane_idx).entry(index) else {
                continue;
            };
            if entry.is_dir || !is_thumbnail_candidate(&entry.name) {
                continue;
            }
            let distance = distance_to_visible(visible_position, visible_start, visible_end);
            if scrolling_fast && distance > 0 {
                continue;
            }
            let key = ThumbnailCacheKey::from_entry(entry);
            if should_queue_thumbnail_for_path(
                key.path(),
                state.pane(pane_idx).view_mode.clone(),
                distance,
            ) {
                desired.insert(key);
            }
        }
    }
    desired
}

fn thumbnail_prefetch_range(state: &AppState, pane_idx: usize, scan_end: usize) -> (usize, usize) {
    let (visible_start, visible_end) = visible_index_range(state, pane_idx, scan_end);
    // Thumbnail extraction only reads headers, so the same prefetch range is used
    // for remote and local files.
    let margin = match state.pane(pane_idx).view_mode {
        ViewMode::List => 48,
        ViewMode::Grid => grid_items_per_row_for_pane(state, pane_idx) * 3,
    };
    let start = visible_start.saturating_sub(margin);
    let end = (visible_end + margin).min(scan_end);
    (start, end.max(start + 1))
}

pub(super) fn thumbnail_priority(path: &str) -> u8 {
    thumbnail_priority_rank(path)
}

pub(super) fn should_queue_thumbnail_for_path(
    path: &str,
    view_mode: ViewMode,
    distance: usize,
) -> bool {
    if view_mode != ViewMode::Grid {
        return false;
    }

    let lower = path.to_lowercase();
    if is_video_path(&lower) {
        return distance == 0 && iris_thumb_preview::video::supports_video_thumbnails();
    }

    true
}

fn is_background_thumbnail(path: &str) -> bool {
    is_background_media_path(path)
}

#[cfg(test)]
mod tests {
    use super::{
        collect_desired_paths, collect_missing_candidates, is_thumbnail_candidate,
        FAST_SCROLL_VELOCITY_PX_PER_SEC,
    };
    use crate::state::{AppState, ViewMode};
    use iris_core_domain::types::list::ListEntry;
    use iris_core_domain::types::metadata::MetadataLevel;
    use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
    use iris_thumb_preview::scheduler::RequestPriority;

    #[test]
    fn pdf_candidate_tracks_feature_support() {
        assert_eq!(
            is_thumbnail_candidate("document.pdf"),
            iris_thumb_preview::pdf::supports_pdf_thumbnails()
        );
    }

    #[test]
    fn raw_candidate_tracks_feature_support() {
        assert_eq!(
            is_thumbnail_candidate("sample.cr3"),
            iris_thumb_preview::raw::supports_raw_extension("cr3")
        );
        assert_eq!(
            is_thumbnail_candidate("sample.raf"),
            iris_thumb_preview::raw::supports_raw_extension("raf")
        );
    }

    #[test]
    fn modern_images_are_thumbnail_candidates() {
        for extension in ["avif", "heic", "jp2", "jxr", "jxl"] {
            assert!(
                is_thumbnail_candidate(&format!("sample.{extension}")),
                "missing candidate support for {extension}"
            );
        }
    }

    fn image_entry(path: &str) -> ListEntry {
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path)
            .to_string();
        ListEntry {
            object_ref: ObjectRef {
                key: ObjectKey {
                    backend_id: BackendId("local".to_string()),
                    stable_id: StableId(path.to_string()),
                },
                parent_stable_id: None,
                display_path: path.to_string(),
            },
            sort_name_key: ListEntry::sort_name_key_for(&name),
            sort_extension_key: ListEntry::sort_extension_key_for(&name),
            name,
            is_dir: false,
            metadata_level: MetadataLevel::L1,
            size: Some(1),
            mtime: None,
            ctime: None,
            is_symlink: false,
            symlink_target: None,
            is_inaccessible: false,
            annotation: None,
        }
    }

    #[test]
    fn thumbnail_candidates_follow_filtered_visible_projection() {
        let mut state = AppState::default();
        state.window_height = 400.0;
        let pane = state.pane_mut(0);
        pane.view_mode = ViewMode::Grid;
        pane.entries = vec![
            image_entry("/tmp/keep-photo.jpg"),
            image_entry("/tmp/hidden-photo.jpg"),
        ];
        pane.set_filter_query("keep".to_string());

        let candidates = collect_missing_candidates(&state);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].3, 0);

        let desired = collect_desired_paths(&state);
        assert!(desired
            .iter()
            .any(|key| key.path() == "/tmp/keep-photo.jpg"));
        assert!(!desired
            .iter()
            .any(|key| key.path() == "/tmp/hidden-photo.jpg"));
    }

    #[test]
    fn fast_scrolling_gates_nearby_thumbnail_candidates() {
        let mut state = AppState::default();
        state.window_width = 800.0;
        state.window_height = 400.0;
        {
            let pane = state.pane_mut(0);
            pane.view_mode = ViewMode::Grid;
            pane.viewport_height = 150.0;
            pane.entries = (0..40)
                .map(|index| image_entry(&format!("/tmp/photo-{index:02}.jpg")))
                .collect();
            pane.rebuild_visible_indices();
        }

        let normal_count = collect_missing_candidates(&state).len();
        {
            let pane = state.pane_mut(0);
            pane.scroll_velocity_y = FAST_SCROLL_VELOCITY_PX_PER_SEC + 1.0;
            pane.last_scroll_at = Some(std::time::Instant::now());
        }
        let fast_candidates = collect_missing_candidates(&state);

        assert!(fast_candidates.len() < normal_count);
        assert!(fast_candidates
            .iter()
            .all(|candidate| candidate.2 == RequestPriority::Visible));
    }
}
