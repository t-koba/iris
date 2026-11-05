use super::*;

fn remove_thumbnail_cache_entry(state: &mut AppState, key: &ThumbnailCacheKey) {
    if let Some(entry) = state.thumbnail_cache.remove(key) {
        state.thumbnail_cache_bytes = state.thumbnail_cache_bytes.saturating_sub(entry.memory_len);
        state.thumbnail_cache_lru.remove(key);
    }
}

fn enforce_thumbnail_cache_limits(state: &mut AppState) {
    while state.thumbnail_cache.len() > MAX_THUMBNAIL_CACHE_ITEMS
        || state.thumbnail_cache_bytes > state.thumbnail_cache_limit_bytes
    {
        let Some(oldest) = state.thumbnail_cache_lru.pop_front() else {
            break;
        };
        if let Some(entry) = state.thumbnail_cache.remove(&oldest) {
            state.thumbnail_cache_bytes =
                state.thumbnail_cache_bytes.saturating_sub(entry.memory_len);
        }
    }
}

pub fn trim_thumbnail_cache_to_limits(state: &mut AppState) {
    enforce_thumbnail_cache_limits(state);
}

pub fn trim_thumbnail_cache_to_memory_budget(state: &mut AppState) {
    let budget = iris_core_domain::memory_budget::MemoryBudget::global();
    while budget.allocated_bytes() > budget.cap_bytes() {
        let Some(oldest) = state.thumbnail_cache_lru.pop_front() else {
            break;
        };
        if let Some(entry) = state.thumbnail_cache.remove(&oldest) {
            state.thumbnail_cache_bytes =
                state.thumbnail_cache_bytes.saturating_sub(entry.memory_len);
        }
    }
}

pub fn clear_thumbnail_cache(state: &mut AppState) {
    if !state.thumbnail_inflight.is_empty() {
        app_runtime()
            .cancel_thumbnails(state.thumbnail_inflight.iter().map(ThumbnailCacheKey::path));
    }
    state.thumbnail_cache.clear();
    state.thumbnail_cache_lru.clear();
    state.thumbnail_cache_bytes = 0;
    state.thumbnail_generation_queue.clear();
    state.thumbnail_generation_queued.clear();
    state.thumbnail_generation_requests.clear();
    state.thumbnail_inflight.clear();
    state.thumbnail_inflight_backends.clear();
    state.thumbnail_canceled.clear();
    state.thumbnail_unsupported.clear();
    state.thumbnail_schedulers.clear();
}

pub fn invalidate_changed_thumbnail_entries(
    state: &mut AppState,
    previous_entries: &[ListEntry],
    new_entries: &[ListEntry],
) {
    let previous_by_path = previous_entries
        .iter()
        .map(|entry| {
            (
                entry.object_ref.display_path.clone(),
                EntryCacheStamp::from_entry(entry),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();

    let changed_paths = new_entries
        .iter()
        .filter_map(|entry| {
            let path = &entry.object_ref.display_path;
            let new_stamp = EntryCacheStamp::from_entry(entry);
            previous_by_path
                .get(path)
                .filter(|previous_stamp| **previous_stamp != new_stamp)
                .map(|_| path.clone())
        })
        .collect::<Vec<_>>();

    invalidate_thumbnail_paths(state, &changed_paths);
}

fn invalidate_thumbnail_paths(state: &mut AppState, paths: &[String]) {
    if paths.is_empty() {
        return;
    }

    app_runtime().cancel_thumbnails(paths.iter().map(String::as_str));

    let path_set = paths
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    for path in &path_set {
        let canceled_keys = state
            .thumbnail_inflight
            .iter()
            .filter(|key| key.path() == path)
            .cloned()
            .collect::<Vec<_>>();
        let stale_cache_keys = state
            .thumbnail_cache
            .keys()
            .filter(|key| key.path() == path)
            .cloned()
            .collect::<Vec<_>>();
        for key in stale_cache_keys {
            remove_thumbnail_cache_entry(state, &key);
        }
        state
            .thumbnail_generation_queued
            .retain(|key| key.path() != path);
        state
            .thumbnail_generation_requests
            .retain(|key, _| key.path() != path);
        state.thumbnail_inflight.retain(|key| key.path() != path);
        state
            .thumbnail_inflight_backends
            .retain(|key, _| key.path() != path);
        state.thumbnail_canceled.retain(|key| key.path() != path);
        state.thumbnail_unsupported.retain(|key| key.path() != path);
        state.thumbnail_canceled.extend(canceled_keys);
    }
    state
        .thumbnail_cache_lru
        .retain(|key| !path_set.contains(key.path()));
    state
        .thumbnail_generation_queue
        .retain(|key| !path_set.contains(key.path()));
}

pub fn stop_thumbnail_work(state: &mut AppState) {
    if !state.thumbnail_inflight.is_empty() {
        app_runtime()
            .cancel_thumbnails(state.thumbnail_inflight.iter().map(ThumbnailCacheKey::path));
        state
            .thumbnail_canceled
            .extend(state.thumbnail_inflight.iter().cloned());
    }
    state.thumbnail_generation_queue.clear();
    state.thumbnail_generation_queued.clear();
    state.thumbnail_generation_requests.clear();
    state.thumbnail_inflight.clear();
    state.thumbnail_inflight_backends.clear();
    state.thumbnail_schedulers.clear();
}

pub fn insert_thumbnail_cache(
    state: &mut AppState,
    key: ThumbnailCacheKey,
    thumbnail: ThumbnailData,
) {
    insert_thumbnail_cache_with_budget(
        state,
        key,
        thumbnail,
        iris_core_domain::memory_budget::MemoryBudget::global(),
    );
}

fn insert_thumbnail_cache_with_budget(
    state: &mut AppState,
    key: ThumbnailCacheKey,
    thumbnail: ThumbnailData,
    budget: std::sync::Arc<iris_core_domain::memory_budget::MemoryBudget>,
) {
    state.thumbnail_unsupported.remove(&key);
    let ThumbnailData { handle, memory_len } = thumbnail;
    remove_thumbnail_cache_entry(state, &key);
    let memory_permit = loop {
        if let Some(permit) = budget.try_acquire(memory_len) {
            break permit;
        }
        let Some(oldest) = state.thumbnail_cache_lru.pop_front() else {
            return;
        };
        if let Some(entry) = state.thumbnail_cache.remove(&oldest) {
            state.thumbnail_cache_bytes =
                state.thumbnail_cache_bytes.saturating_sub(entry.memory_len);
        }
    };
    let entry = CachedThumbnail {
        handle,
        memory_len,
        _memory_permit: memory_permit,
    };
    state.thumbnail_cache.insert(key.clone(), entry);
    state.thumbnail_cache_lru.touch(key);
    state.thumbnail_cache_bytes += memory_len;
    enforce_thumbnail_cache_limits(state);
}

pub fn touch_thumbnail_cache_entry(state: &mut AppState, key: &ThumbnailCacheKey) {
    if state.thumbnail_cache.contains_key(key) {
        state.thumbnail_cache_lru.touch(key.clone());
    }
}

pub fn thumbnail_cache_key_is_current(state: &AppState, key: &ThumbnailCacheKey) -> bool {
    state.active_panes().into_iter().any(|pane| {
        pane.entry_by_display_path(key.path())
            .is_some_and(|entry| ThumbnailCacheKey::from_entry(entry) == *key)
    })
}

fn stale_thumbnail_keys<'a, I>(state: &AppState, keys: I) -> Vec<ThumbnailCacheKey>
where
    I: IntoIterator<Item = &'a ThumbnailCacheKey>,
{
    keys.into_iter()
        .filter(|key| !thumbnail_cache_key_is_current(state, key))
        .cloned()
        .collect()
}

/// Prunes the cache to the entries shown in any pane.
pub fn prune_thumbnail_cache(state: &mut AppState) {
    let canceled = stale_thumbnail_keys(state, state.thumbnail_inflight.iter());
    if !canceled.is_empty() {
        app_runtime().cancel_thumbnails(canceled.iter().map(ThumbnailCacheKey::path));
        for key in &canceled {
            state.thumbnail_inflight.remove(key);
            state.thumbnail_inflight_backends.remove(key);
            state.thumbnail_canceled.insert(key.clone());
        }
    }

    let stale_inflight_backends =
        stale_thumbnail_keys(state, state.thumbnail_inflight_backends.keys())
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
    state
        .thumbnail_inflight_backends
        .retain(|key, _| !stale_inflight_backends.contains(key));

    let stale_cache_keys = stale_thumbnail_keys(state, state.thumbnail_cache.keys());
    for key in stale_cache_keys {
        remove_thumbnail_cache_entry(state, &key);
    }
    let stale_canceled = stale_thumbnail_keys(state, state.thumbnail_canceled.iter())
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    state
        .thumbnail_canceled
        .retain(|key| !stale_canceled.contains(key));

    let stale_unsupported = stale_thumbnail_keys(state, state.thumbnail_unsupported.iter())
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    state
        .thumbnail_unsupported
        .retain(|key| !stale_unsupported.contains(key));

    let stale_queued = stale_thumbnail_keys(state, state.thumbnail_generation_queued.iter())
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    for key in &stale_queued {
        state.thumbnail_generation_requests.remove(key);
    }
    state
        .thumbnail_generation_queued
        .retain(|key| !stale_queued.contains(key));
    state
        .thumbnail_generation_queue
        .retain(|key| !stale_queued.contains(key));

    let stale_generation_queue =
        stale_thumbnail_keys(state, state.thumbnail_generation_queue.iter())
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
    state
        .thumbnail_generation_queue
        .retain(|key| !stale_generation_queue.contains(key));

    let stale_requests = stale_thumbnail_keys(state, state.thumbnail_generation_requests.keys())
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    state
        .thumbnail_generation_requests
        .retain(|key, _| !stale_requests.contains(key));
    state.thumbnail_schedulers.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::image as iced_image;

    use iris_core_domain::types::object::{BackendId, ObjectKey, StableId};
    use std::time::{Duration, SystemTime};

    fn thumbnail(memory_len: usize) -> ThumbnailData {
        ThumbnailData {
            handle: iced_image::Handle::from_bytes(Vec::new()),
            memory_len,
        }
    }

    fn key(path: &str, size: u64) -> ThumbnailCacheKey {
        ThumbnailCacheKey {
            path: path.to_string(),
            size: Some(size),
            mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(size)),
        }
    }

    fn entry(path: &str, size: u64) -> ListEntry {
        ListEntry {
            size: Some(size),
            mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(size)),
            ..crate::test_support::local_entry(path, false)
        }
    }

    #[test]
    fn thumbnail_cache_touch_preserves_recent_entry_during_eviction() {
        let mut state = AppState::default();
        state.thumbnail_cache_limit_bytes = 2;

        let a = key("/tmp/a.jpg", 1);
        let b = key("/tmp/b.jpg", 1);
        let c = key("/tmp/c.jpg", 1);

        insert_thumbnail_cache(&mut state, a.clone(), thumbnail(1));
        insert_thumbnail_cache(&mut state, b.clone(), thumbnail(1));
        touch_thumbnail_cache_entry(&mut state, &a);
        insert_thumbnail_cache(&mut state, c.clone(), thumbnail(1));

        assert!(state.thumbnail_cache.contains_key(&a));
        assert!(!state.thumbnail_cache.contains_key(&b));
        assert!(state.thumbnail_cache.contains_key(&c));
        assert_eq!(state.thumbnail_cache_bytes, 2);
    }

    #[test]
    fn thumbnail_cache_evicts_against_shared_memory_budget() {
        let budget = std::sync::Arc::new(iris_core_domain::memory_budget::MemoryBudget::new(
            128 * 1024,
        ));
        let mut state = AppState::default();
        state.thumbnail_cache_limit_bytes = usize::MAX;
        let first = key("/tmp/first.jpg", 1);
        let second = key("/tmp/second.jpg", 2);

        insert_thumbnail_cache_with_budget(
            &mut state,
            first.clone(),
            thumbnail(96 * 1024),
            std::sync::Arc::clone(&budget),
        );
        insert_thumbnail_cache_with_budget(
            &mut state,
            second.clone(),
            thumbnail(96 * 1024),
            std::sync::Arc::clone(&budget),
        );

        assert!(!state.thumbnail_cache.contains_key(&first));
        assert!(state.thumbnail_cache.contains_key(&second));
        assert_eq!(budget.allocated_bytes(), 96 * 1024);
        clear_thumbnail_cache(&mut state);
        assert_eq!(budget.allocated_bytes(), 0);
    }

    #[test]
    fn clear_thumbnail_cache_resets_lru() {
        let mut state = AppState::default();
        insert_thumbnail_cache(&mut state, key("/tmp/a.jpg", 1), thumbnail(1));
        assert!(!state.thumbnail_cache_lru.is_empty());

        clear_thumbnail_cache(&mut state);

        assert!(state.thumbnail_cache_lru.is_empty());
    }

    #[test]
    fn thumbnail_cache_touch_reuses_existing_lru_node() {
        let mut state = AppState::default();
        let cache_key = key("/tmp/a.jpg", 1);
        insert_thumbnail_cache(&mut state, cache_key.clone(), thumbnail(1));

        for _ in 0..900 {
            touch_thumbnail_cache_entry(&mut state, &cache_key);
        }

        assert!(state.thumbnail_cache.contains_key(&cache_key));
        assert_eq!(state.thumbnail_cache_lru.len(), 1);
    }

    #[test]
    fn thumbnail_cache_key_separates_reused_paths_by_stamp() {
        let mut state = AppState::default();
        let old_key = key("/tmp/a.jpg", 1);
        let new_key = key("/tmp/a.jpg", 2);

        insert_thumbnail_cache(&mut state, old_key.clone(), thumbnail(1));
        insert_thumbnail_cache(&mut state, new_key.clone(), thumbnail(1));

        assert!(state.thumbnail_cache.contains_key(&old_key));
        assert!(state.thumbnail_cache.contains_key(&new_key));
    }

    #[test]
    fn thumbnail_cache_key_current_requires_matching_stamp() {
        let mut state = AppState::default();
        state.pane_mut(0).entries = vec![entry("/tmp/a.jpg", 2)];

        assert!(!thumbnail_cache_key_is_current(
            &state,
            &key("/tmp/a.jpg", 1)
        ));
        assert!(thumbnail_cache_key_is_current(
            &state,
            &key("/tmp/a.jpg", 2)
        ));
    }

    #[test]
    fn prune_thumbnail_cache_removes_stale_tracked_keys() {
        let mut state = AppState::default();
        let current_entry = entry("/tmp/a.jpg", 1);
        let current_key = ThumbnailCacheKey::from_entry(&current_entry);
        let stale_key = key("/tmp/b.jpg", 2);
        state.pane_mut(0).replace_entries(vec![current_entry]);

        insert_thumbnail_cache(&mut state, current_key.clone(), thumbnail(1));
        insert_thumbnail_cache(&mut state, stale_key.clone(), thumbnail(1));
        state.thumbnail_inflight.insert(stale_key.clone());
        state
            .thumbnail_inflight_backends
            .insert(stale_key.clone(), "local".to_string());
        state.thumbnail_canceled.insert(stale_key.clone());
        state.thumbnail_unsupported.insert(stale_key.clone());
        state
            .thumbnail_generation_queue
            .push_back(stale_key.clone());
        state.thumbnail_generation_queued.insert(stale_key.clone());
        state.thumbnail_generation_requests.insert(
            stale_key.clone(),
            crate::state::QueuedThumbnailRequest {
                object_key: ObjectKey {
                    backend_id: BackendId("local".to_string()),
                    stable_id: StableId("/tmp/b.jpg".to_string()),
                },
                backend_type: "local".to_string(),
                priority: RequestPriority::Nearby,
            },
        );

        prune_thumbnail_cache(&mut state);

        assert!(state.thumbnail_cache.contains_key(&current_key));
        assert!(!state.thumbnail_cache.contains_key(&stale_key));
        assert!(!state.thumbnail_inflight.contains(&stale_key));
        assert!(!state.thumbnail_inflight_backends.contains_key(&stale_key));
        assert!(!state.thumbnail_canceled.contains(&stale_key));
        assert!(!state.thumbnail_unsupported.contains(&stale_key));
        assert!(!state.thumbnail_generation_queue.contains(&stale_key));
        assert!(!state.thumbnail_generation_queued.contains(&stale_key));
        assert!(!state.thumbnail_generation_requests.contains_key(&stale_key));
    }
}
