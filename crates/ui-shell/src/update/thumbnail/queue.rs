use super::cache::touch_thumbnail_cache_entry;
use super::candidates::{collect_desired_paths, collect_missing_candidates};
use super::support::{
    backend_type_for_object, backend_type_from_backend_id, generate_one_thumbnail,
    is_network_backend_type, preview_throttled_thumbnail_backend, ThumbnailTaskOutcome,
};
use super::*;
use crate::state::QueuedThumbnailRequest;

pub fn generate_thumbnails(state: &mut AppState) -> Task<Message> {
    let preview_blocked_backend = preview_throttled_thumbnail_backend(state);

    let mut inflight_by_backend = std::collections::HashMap::<String, usize>::new();
    for key in &state.thumbnail_inflight {
        let backend_type = state
            .thumbnail_inflight_backends
            .get(key)
            .cloned()
            .unwrap_or_else(|| {
                backend_type_from_backend_id(
                    &app_runtime().backend_id_for_path(std::path::Path::new(key.path())),
                )
            });
        *inflight_by_backend.entry(backend_type).or_insert(0) += 1;
    }

    let mut tasks = Vec::new();
    let mut schedulers = state
        .thumbnail_schedulers
        .iter()
        .map(|(backend_type, scheduler)| {
            (
                backend_type.clone(),
                scheduler.has_visible_pending(),
                scheduler.pending_count(),
                scheduler.max_workers(),
            )
        })
        .collect::<Vec<_>>();
    schedulers.sort_by_key(|(backend_type, has_visible_pending, pending_count, _)| {
        (
            !has_visible_pending,
            !is_network_backend_type(backend_type),
            *pending_count,
        )
    });
    let available_slots = schedulers
        .iter()
        .map(|(_, _, _, max_workers)| *max_workers)
        .sum::<usize>()
        .saturating_sub(state.thumbnail_inflight.len());
    if available_slots == 0 {
        return Task::none();
    }
    for (backend_type, _, _, mut max_for_backend) in schedulers {
        // While a preview loads, halve (but do not block) the workers on the same backend.
        if let Some(ref blocked) = preview_blocked_backend {
            if backend_type == *blocked {
                max_for_backend = (max_for_backend / 2).max(1);
            }
        }
        let mut started_for_backend = 0usize;
        let already_inflight = inflight_by_backend.get(&backend_type).copied().unwrap_or(0);

        while tasks.len() < available_slots
            && already_inflight + started_for_backend < max_for_backend
        {
            let Some(request) = state
                .thumbnail_schedulers
                .get_mut(&backend_type)
                .and_then(ThumbnailScheduler::dequeue)
            else {
                break;
            };
            let Some(entry) = thumbnail_entry_for_object_key(state, &request.object_key) else {
                continue;
            };
            let entry = entry.clone();

            let key = ThumbnailCacheKey::from_entry(&entry);
            state.thumbnail_generation_queued.remove(&key);
            state.thumbnail_generation_requests.remove(&key);
            state
                .thumbnail_generation_queue
                .retain(|queued| queued != &key);

            if state.thumbnail_cache.contains_key(&key) || state.thumbnail_inflight.contains(&key) {
                touch_thumbnail_cache_entry(state, &key);
                continue;
            }
            if state.thumbnail_unsupported.contains(&key) {
                continue;
            }

            state.thumbnail_canceled.remove(&key);
            state.thumbnail_inflight.insert(key.clone());
            state
                .thumbnail_inflight_backends
                .insert(key.clone(), backend_type.clone());
            started_for_backend += 1;
            tasks.push(Task::perform(
                generate_one_thumbnail(entry.object_ref.clone(), request.content_type.clone()),
                {
                    let key_for_msg = key.clone();
                    move |result| match result {
                        ThumbnailTaskOutcome::Ready(data) => {
                            Message::ThumbnailReady(key_for_msg, data)
                        }
                        ThumbnailTaskOutcome::Unsupported => {
                            Message::ThumbnailUnsupported(key_for_msg)
                        }
                        ThumbnailTaskOutcome::Failed => Message::ThumbnailFailed(key_for_msg),
                    }
                },
            ));
        }
    }

    if tasks.is_empty() {
        Task::none()
    } else {
        Task::batch(tasks)
    }
}

fn thumbnail_entry_for_object_key<'a>(
    state: &'a AppState,
    object_key: &ObjectKey,
) -> Option<&'a ListEntry> {
    for pane_idx in 0..2 {
        let pane = state.pane(pane_idx);
        if let Some(index) = pane.entry_index_by_object_key(object_key) {
            return pane.entry(index);
        }
    }
    None
}

pub fn seed_thumbnail_generation_queue(state: &mut AppState) {
    state.thumbnail_schedulers.clear();
    state.thumbnail_generation_queue.clear();
    state.thumbnail_generation_queued.clear();
    state.thumbnail_generation_requests.clear();

    for (_, _, priority, index, pane_idx) in collect_missing_candidates(state) {
        if let Some(entry) = state.pane(pane_idx).entry(index).cloned() {
            enqueue_thumbnail_request(state, &entry, priority);
        }
    }
}

/// Cancels off-screen work and enqueues what is missing for the visible ranges.
pub fn sync_thumbnail_generation_queue(state: &mut AppState) {
    let desired_keys = collect_desired_paths(state);
    cancel_offscreen_thumbnail_work(state, &desired_keys);

    for (_, _, priority, index, pane_idx) in collect_missing_candidates(state) {
        if let Some(entry) = state.pane(pane_idx).entry(index).cloned() {
            enqueue_thumbnail_request(state, &entry, priority);
        }
    }
}

fn enqueue_thumbnail_request(state: &mut AppState, entry: &ListEntry, priority: RequestPriority) {
    let key = ThumbnailCacheKey::from_entry(entry);
    if state.thumbnail_cache.contains_key(&key)
        || state.thumbnail_inflight.contains(&key)
        || state.thumbnail_unsupported.contains(&key)
    {
        return;
    }

    let backend_type = backend_type_for_object(&entry.object_ref);
    let content_type = mime_guess::from_path(key.path())
        .first()
        .map(|mime| mime.to_string());
    let queued = state
        .thumbnail_schedulers
        .entry(backend_type.clone())
        .or_insert_with(|| ThumbnailScheduler::new_for_backend(&backend_type))
        .enqueue(ThumbnailRequest {
            object_key: entry.object_ref.key.clone(),
            hint: ThumbnailHint {
                max_dimension: THUMB_SIZE,
                preferred_formats: vec![ImageFormat::Jpeg, ImageFormat::WebP, ImageFormat::Png],
            },
            content_type,
            priority,
        });
    if !queued {
        return;
    }
    if state.thumbnail_generation_queued.contains(&key) {
        if let Some(existing) = state.thumbnail_generation_requests.get_mut(&key) {
            existing.priority = existing.priority.min(priority);
        }
        return;
    }
    state.thumbnail_generation_queue.push_back(key.clone());
    state.thumbnail_generation_queued.insert(key.clone());
    state.thumbnail_generation_requests.insert(
        key,
        QueuedThumbnailRequest {
            object_key: entry.object_ref.key.clone(),
            backend_type,
            priority,
        },
    );
}

/// In-flight remote downloads are kept and cached to avoid wasting bandwidth.
fn cancel_offscreen_thumbnail_work(
    state: &mut AppState,
    desired_keys: &std::collections::HashSet<ThumbnailCacheKey>,
) {
    let canceled_inflight: Vec<ThumbnailCacheKey> = state
        .thumbnail_inflight
        .iter()
        .filter(|key| {
            if desired_keys.contains(*key) {
                return false;
            }
            let backend_id = app_runtime().backend_id_for_path(Path::new(key.path()));
            let bt = backend_type_from_backend_id(&backend_id);
            !is_network_backend_type(&bt)
        })
        .cloned()
        .collect();
    if !canceled_inflight.is_empty() {
        app_runtime().cancel_thumbnails(canceled_inflight.iter().map(ThumbnailCacheKey::path));
        for key in &canceled_inflight {
            state.thumbnail_inflight.remove(key);
            state.thumbnail_inflight_backends.remove(key);
            state.thumbnail_canceled.insert(key.clone());
        }
    }

    let mut queued_by_backend = std::collections::HashMap::<String, Vec<ObjectKey>>::new();
    let keys_to_remove: Vec<ThumbnailCacheKey> = state
        .thumbnail_generation_queued
        .iter()
        .filter(|key| !desired_keys.contains(*key))
        .cloned()
        .collect();
    let keys_to_remove_set = keys_to_remove
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    for key in &keys_to_remove {
        state.thumbnail_generation_queued.remove(key);
        if let Some(request) = state.thumbnail_generation_requests.remove(key) {
            queued_by_backend
                .entry(request.backend_type)
                .or_default()
                .push(request.object_key);
        }
    }
    state
        .thumbnail_generation_queue
        .retain(|key| desired_keys.contains(key) || !keys_to_remove_set.contains(key));
    for (backend_type, keys) in queued_by_backend {
        if let Some(scheduler) = state.thumbnail_schedulers.get_mut(&backend_type) {
            scheduler.cancel_batch(&keys);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core_domain::types::metadata::MetadataLevel;
    use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};

    fn image_entry(path: &str) -> ListEntry {
        let name = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path)
            .to_string();
        ListEntry {
            metadata_level: MetadataLevel::L1,
            size: Some(1),
            ..ListEntry::new(
                ObjectRef {
                    key: ObjectKey {
                        backend_id: BackendId("local".to_string()),
                        stable_id: StableId(path.to_string()),
                    },
                    parent_stable_id: None,
                    display_path: path.to_string(),
                },
                name,
                false,
            )
        }
    }

    #[test]
    fn queued_thumbnail_requests_record_scheduler_metadata() {
        let mut state = AppState::default();
        state.window_height = 400.0;
        {
            let pane = state.pane_mut(0);
            pane.view_mode = ViewMode::Grid;
            pane.entries = vec![image_entry("/tmp/keep-photo.jpg")];
            pane.rebuild_visible_indices();
        }

        seed_thumbnail_generation_queue(&mut state);

        assert_eq!(state.thumbnail_generation_queued.len(), 1);
        assert_eq!(state.thumbnail_generation_requests.len(), 1);
        let key = state.thumbnail_generation_queued.iter().next().unwrap();
        let request = state.thumbnail_generation_requests.get(key).unwrap();
        assert_eq!(request.object_key, state.pane(0).entries[0].object_ref.key);
        assert!(!request.backend_type.is_empty());
    }

    #[test]
    fn sync_thumbnail_generation_queue_removes_offscreen_request_metadata() {
        let mut state = AppState::default();
        state.window_height = 400.0;
        {
            let pane = state.pane_mut(0);
            pane.view_mode = ViewMode::Grid;
            pane.entries = vec![image_entry("/tmp/keep-photo.jpg")];
            pane.rebuild_visible_indices();
        }
        seed_thumbnail_generation_queue(&mut state);

        state.pane_mut(0).set_filter_query("missing".to_string());
        sync_thumbnail_generation_queue(&mut state);

        assert!(state.thumbnail_generation_queue.is_empty());
        assert!(state.thumbnail_generation_queued.is_empty());
        assert!(state.thumbnail_generation_requests.is_empty());
    }

    #[test]
    fn queued_nearby_request_is_promoted_when_it_becomes_visible() {
        let mut state = AppState::default();
        let entry = image_entry("/tmp/promoted-photo.jpg");

        enqueue_thumbnail_request(&mut state, &entry, RequestPriority::Nearby);
        enqueue_thumbnail_request(&mut state, &entry, RequestPriority::Visible);

        let key = ThumbnailCacheKey::from_entry(&entry);
        assert_eq!(
            state.thumbnail_generation_requests[&key].priority,
            RequestPriority::Visible
        );
        let scheduler = state.thumbnail_schedulers.get_mut("local").unwrap();
        assert_eq!(scheduler.pending_count(), 1);
        assert_eq!(
            scheduler.dequeue().map(|request| request.priority),
            Some(RequestPriority::Visible)
        );
    }
}
