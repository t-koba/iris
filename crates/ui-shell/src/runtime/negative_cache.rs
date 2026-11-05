use super::{AppRuntime, NEGATIVE_PATH_CACHE_TTL};
use iris_core_domain::types::object::BackendId;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum NegativeCacheKind {
    MissingPath,
    EmptyDirectory,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct NegativeCacheKey {
    backend_id: String,
    display_path: String,
    kind: NegativeCacheKind,
}

#[derive(Default)]
pub(super) struct NegativePathCache {
    entries: HashMap<NegativeCacheKey, Instant>,
}

impl NegativePathCache {
    pub(super) fn contains(
        &mut self,
        backend_id: &str,
        display_path: &str,
        kind: NegativeCacheKind,
        now: Instant,
    ) -> bool {
        let key = NegativeCacheKey {
            backend_id: backend_id.to_string(),
            display_path: display_path.to_string(),
            kind,
        };
        match self.entries.get(&key).copied() {
            Some(expires_at) if expires_at > now => true,
            Some(_) => {
                self.entries.remove(&key);
                false
            }
            None => false,
        }
    }

    pub(super) fn insert(
        &mut self,
        backend_id: &str,
        display_path: &str,
        kind: NegativeCacheKind,
        now: Instant,
        ttl: Duration,
    ) {
        self.prune_expired(now);
        self.entries.insert(
            NegativeCacheKey {
                backend_id: backend_id.to_string(),
                display_path: display_path.to_string(),
                kind,
            },
            now + ttl,
        );
    }

    pub(super) fn remove_path(&mut self, backend_id: &str, display_path: &str) {
        self.entries
            .retain(|key, _| key.backend_id != backend_id || key.display_path != display_path);
    }

    pub(super) fn remove_prefix(&mut self, backend_id: &str, display_path: &str) {
        let descendant_prefix = if display_path.is_empty() {
            None
        } else {
            Some(format!("{display_path}/"))
        };
        self.entries.retain(|key, _| {
            if key.backend_id != backend_id {
                return true;
            }
            key.display_path != display_path
                && !descendant_prefix
                    .as_ref()
                    .is_some_and(|prefix| key.display_path.starts_with(prefix))
        });
    }

    fn prune_expired(&mut self, now: Instant) {
        self.entries.retain(|_, expires_at| *expires_at > now);
    }
}

impl AppRuntime {
    pub(super) fn negative_cache_contains(
        &self,
        backend_id: &BackendId,
        display_path: &str,
        kind: NegativeCacheKind,
    ) -> bool {
        self.negative_path_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains(&backend_id.0, display_path, kind, Instant::now())
    }

    pub(super) fn remember_negative_path(
        &self,
        backend_id: &BackendId,
        display_path: &str,
        kind: NegativeCacheKind,
    ) {
        self.negative_path_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                &backend_id.0,
                display_path,
                kind,
                Instant::now(),
                NEGATIVE_PATH_CACHE_TTL,
            );
    }

    pub(super) fn clear_negative_path(&self, backend_id: &BackendId, display_path: &str) {
        self.negative_path_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove_path(&backend_id.0, display_path);
    }

    pub(super) fn clear_negative_path_prefix(&self, backend_id: &BackendId, display_path: &str) {
        self.negative_path_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove_prefix(&backend_id.0, display_path);
    }

    pub(super) fn clear_parent_empty_cache(&self, backend_id: &BackendId, display_path: &str) {
        let Some(parent) = Path::new(display_path)
            .parent()
            .map(|parent| parent.to_string_lossy().to_string())
            .filter(|parent| !parent.is_empty())
        else {
            return;
        };
        self.clear_negative_path(backend_id, &parent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_cache_entries_expire() {
        let mut cache = NegativePathCache::default();
        let now = Instant::now();
        cache.insert(
            "local",
            "/tmp/missing",
            NegativeCacheKind::MissingPath,
            now,
            Duration::from_secs(2),
        );

        assert!(cache.contains(
            "local",
            "/tmp/missing",
            NegativeCacheKind::MissingPath,
            now + Duration::from_secs(1),
        ));
        assert!(!cache.contains(
            "local",
            "/tmp/missing",
            NegativeCacheKind::MissingPath,
            now + Duration::from_secs(3),
        ));
    }

    #[test]
    fn remove_prefix_clears_descendants_only_for_backend() {
        let mut cache = NegativePathCache::default();
        let now = Instant::now();
        for (backend_id, display_path) in [
            ("remote-a", "/root"),
            ("remote-a", "/root/child"),
            ("remote-a", "/rooted/child"),
            ("remote-b", "/root/child"),
        ] {
            cache.insert(
                backend_id,
                display_path,
                NegativeCacheKind::EmptyDirectory,
                now,
                Duration::from_secs(30),
            );
        }

        cache.remove_prefix("remote-a", "/root");

        assert!(!cache.contains("remote-a", "/root", NegativeCacheKind::EmptyDirectory, now,));
        assert!(!cache.contains(
            "remote-a",
            "/root/child",
            NegativeCacheKind::EmptyDirectory,
            now,
        ));
        assert!(cache.contains(
            "remote-a",
            "/rooted/child",
            NegativeCacheKind::EmptyDirectory,
            now,
        ));
        assert!(cache.contains(
            "remote-b",
            "/root/child",
            NegativeCacheKind::EmptyDirectory,
            now,
        ));
    }
}
