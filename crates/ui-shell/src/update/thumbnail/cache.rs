use super::*;
use iris_core_domain::path_identity::equality_key;

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
    let new_keys_by_identity = new_entries
        .iter()
        .map(|entry| {
            (
                (
                    entry.object_ref.key.clone(),
                    equality_key(&entry.object_ref.display_path),
                ),
                ThumbnailCacheKey::from_entry(entry),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    let stale_keys = previous_entries
        .iter()
        .filter_map(|entry| {
            let previous_key = ThumbnailCacheKey::from_entry(entry);
            new_keys_by_identity
                .get(&(
                    entry.object_ref.key.clone(),
                    equality_key(&entry.object_ref.display_path),
                ))
                .filter(|new_key| **new_key != previous_key)
                .map(|_| previous_key)
        })
        .collect::<std::collections::HashSet<_>>();
    if stale_keys.is_empty() {
        return;
    }

    app_runtime().cancel_thumbnails(stale_keys.iter().map(ThumbnailCacheKey::path));
    for key in &stale_keys {
        remove_thumbnail_cache_entry(state, key);
    }
    state
        .thumbnail_inflight
        .retain(|key| !stale_keys.contains(key));
    state
        .thumbnail_generation_queued
        .retain(|key| !stale_keys.contains(key));
    state
        .thumbnail_generation_requests
        .retain(|key, _| !stale_keys.contains(key));
    state
        .thumbnail_inflight_backends
        .retain(|key, _| !stale_keys.contains(key));
    state
        .thumbnail_canceled
        .retain(|key| !stale_keys.contains(key));
    state
        .thumbnail_unsupported
        .retain(|key| !stale_keys.contains(key));
    state
        .thumbnail_cache_lru
        .retain(|key| !stale_keys.contains(key));
    state
        .thumbnail_generation_queue
        .retain(|key| !stale_keys.contains(key));
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
    // An object key is not necessarily unique: hard links share one object
    // identity while appearing under different paths. Validate the exact full
    // key against every entry for that object instead of a single indexed hit.
    state.active_panes().into_iter().any(|pane| {
        pane.entries.iter().any(|entry| {
            entry.object_ref.key == key.object_key && ThumbnailCacheKey::from_entry(entry) == *key
        })
    })
}

fn current_thumbnail_keys(state: &AppState) -> std::collections::HashSet<ThumbnailCacheKey> {
    state
        .active_panes()
        .into_iter()
        .flat_map(|pane| pane.entries.iter())
        .map(ThumbnailCacheKey::from_entry)
        .collect()
}

fn stale_thumbnail_keys<'a, I>(
    keys: I,
    current: &std::collections::HashSet<ThumbnailCacheKey>,
) -> Vec<ThumbnailCacheKey>
where
    I: IntoIterator<Item = &'a ThumbnailCacheKey>,
{
    keys.into_iter()
        .filter(|key| !current.contains(*key))
        .cloned()
        .collect()
}

/// Prunes the cache to the entries shown in any pane.
pub fn prune_thumbnail_cache(state: &mut AppState) {
    let current = current_thumbnail_keys(state);
    let canceled = stale_thumbnail_keys(state.thumbnail_inflight.iter(), &current);
    if !canceled.is_empty() {
        app_runtime().cancel_thumbnails(canceled.iter().map(ThumbnailCacheKey::path));
        for key in &canceled {
            state.thumbnail_inflight.remove(key);
            state.thumbnail_inflight_backends.remove(key);
            state.thumbnail_canceled.insert(key.clone());
        }
    }

    let stale_inflight_backends =
        stale_thumbnail_keys(state.thumbnail_inflight_backends.keys(), &current)
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
    state
        .thumbnail_inflight_backends
        .retain(|key, _| !stale_inflight_backends.contains(key));

    let stale_cache_keys = stale_thumbnail_keys(state.thumbnail_cache.keys(), &current);
    for key in stale_cache_keys {
        remove_thumbnail_cache_entry(state, &key);
    }
    let stale_canceled = stale_thumbnail_keys(state.thumbnail_canceled.iter(), &current)
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    state
        .thumbnail_canceled
        .retain(|key| !stale_canceled.contains(key));

    let stale_unsupported = stale_thumbnail_keys(state.thumbnail_unsupported.iter(), &current)
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    state
        .thumbnail_unsupported
        .retain(|key| !stale_unsupported.contains(key));

    let stale_queued = stale_thumbnail_keys(state.thumbnail_generation_queued.iter(), &current)
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
        stale_thumbnail_keys(state.thumbnail_generation_queue.iter(), &current)
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
    state
        .thumbnail_generation_queue
        .retain(|key| !stale_generation_queue.contains(key));

    let stale_requests = stale_thumbnail_keys(state.thumbnail_generation_requests.keys(), &current)
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
            object_key: ObjectKey {
                backend_id: BackendId("local".into()),
                stable_id: StableId(path.to_string()),
            },
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

    #[test]
    fn invalidation_removes_only_previous_keys_with_matching_identity() {
        // Byte-identical paths exercise this on every platform; NFC/NFD paths
        // additionally exercise normalization-insensitive identity on macOS.
        for new_path in ["Café.jpg", "Cafe\u{301}.jpg"] {
            let mut state = AppState::default();
            let mut previous = entry("Café.jpg", 5);
            previous.object_ref.key.backend_id = BackendId("s3:bucket".into());
            let mut changed = previous.clone();
            changed.object_ref.display_path = new_path.into();
            changed.size = Some(6);
            let mut distinct = previous.clone();
            distinct.object_ref.key.stable_id = StableId("other-object".into());
            distinct.object_ref.display_path = new_path.into();
            let mut other_path = previous.clone();
            other_path.object_ref.display_path = "other.jpg".into();
            let old_key = ThumbnailCacheKey::from_entry(&previous);
            let new_key = ThumbnailCacheKey::from_entry(&changed);
            let distinct_key = ThumbnailCacheKey::from_entry(&distinct);
            let other_path_key = ThumbnailCacheKey::from_entry(&other_path);
            let keys = [&old_key, &new_key, &distinct_key, &other_path_key];
            for key in keys {
                insert_thumbnail_cache(&mut state, key.clone(), thumbnail(1));
                state.thumbnail_inflight.insert(key.clone());
                state
                    .thumbnail_inflight_backends
                    .insert(key.clone(), "s3".into());
                state.thumbnail_canceled.insert(key.clone());
                state.thumbnail_unsupported.insert(key.clone());
                state.thumbnail_generation_queue.push_back(key.clone());
                state.thumbnail_generation_queued.insert(key.clone());
                state.thumbnail_generation_requests.insert(
                    key.clone(),
                    crate::state::QueuedThumbnailRequest {
                        object_key: key.object_key.clone(),
                        backend_type: "s3".into(),
                        priority: RequestPriority::Nearby,
                    },
                );
            }

            let should_invalidate = new_path == "Café.jpg" || cfg!(target_os = "macos");
            invalidate_changed_thumbnail_entries(
                &mut state,
                &[previous, distinct.clone(), other_path.clone()],
                &[changed, distinct, other_path],
            );

            for key in keys {
                let retained = key != &old_key || !should_invalidate;
                assert_eq!(state.thumbnail_cache.contains_key(key), retained);
                assert_eq!(state.thumbnail_inflight.contains(key), retained);
                assert_eq!(
                    state.thumbnail_inflight_backends.contains_key(key),
                    retained
                );
                assert_eq!(state.thumbnail_canceled.contains(key), retained);
                assert_eq!(state.thumbnail_unsupported.contains(key), retained);
                assert_eq!(state.thumbnail_generation_queue.contains(key), retained);
                assert_eq!(state.thumbnail_generation_queued.contains(key), retained);
                assert_eq!(
                    state.thumbnail_generation_requests.contains_key(key),
                    retained
                );
            }
            let expected_keys = keys
                .into_iter()
                .filter(|key| *key != &old_key || !should_invalidate)
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(state.thumbnail_cache_bytes, expected_keys.len());
            assert_eq!(state.thumbnail_cache_lru.len(), expected_keys.len());
            for key in expected_keys {
                assert_eq!(state.thumbnail_cache_lru.pop_front(), Some(key));
            }
        }
    }

    #[test]
    fn current_key_accepts_every_path_of_a_shared_object() {
        let mut state = AppState::default();
        let first = entry("/tmp/first.jpg", 5);
        let mut second = entry("/tmp/second.jpg", 5);
        second.object_ref.key = first.object_ref.key.clone();
        let first_key = ThumbnailCacheKey::from_entry(&first);
        let second_key = ThumbnailCacheKey::from_entry(&second);
        assert_ne!(first_key, second_key);
        state.pane_mut(0).replace_entries(vec![first, second]);

        assert!(thumbnail_cache_key_is_current(&state, &first_key));
        assert!(thumbnail_cache_key_is_current(&state, &second_key));

        insert_thumbnail_cache(&mut state, first_key.clone(), thumbnail(1));
        insert_thumbnail_cache(&mut state, second_key.clone(), thumbnail(1));
        prune_thumbnail_cache(&mut state);
        assert!(state.thumbnail_cache.contains_key(&first_key));
        assert!(state.thumbnail_cache.contains_key(&second_key));
    }

    #[test]
    fn current_key_uses_object_identity_and_path_equality() {
        let mut state = AppState::default();
        let original = entry("Café.jpg", 5);
        let original_key = ThumbnailCacheKey::from_entry(&original);
        let mut respelled = original.clone();
        respelled.object_ref.display_path = "Cafe\u{301}.jpg".into();
        let respelled_key = ThumbnailCacheKey::from_entry(&respelled);
        let mut distinct = original.clone();
        distinct.object_ref.key.backend_id = BackendId("s3:bucket".into());
        distinct.object_ref.display_path = respelled.object_ref.display_path.clone();
        // A display-path lookup would find the distinct object instead, or miss
        // the original object entirely after a normalization-only spelling change.
        state.pane_mut(0).replace_entries(vec![distinct, respelled]);

        assert!(thumbnail_cache_key_is_current(&state, &respelled_key));
        assert_eq!(
            thumbnail_cache_key_is_current(&state, &original_key),
            cfg!(target_os = "macos")
        );
        let mut stale = respelled_key;
        stale.size = Some(6);
        assert!(!thumbnail_cache_key_is_current(&state, &stale));
    }
}
