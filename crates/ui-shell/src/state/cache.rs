use iced::widget::image as iced_image;
use iris_core_domain::memory_budget::MemoryBudgetPermit;
use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::metadata::MediaMetadata;
use iris_core_domain::types::object::ObjectKey;
use iris_thumb_preview::scheduler::RequestPriority;
use std::collections::HashMap;
use std::time::SystemTime;

pub const DEFAULT_THUMBNAIL_CACHE_LIMIT_BYTES: usize = 48 * 1024 * 1024;
pub const DEFAULT_MEMORY_BUDGET_LIMIT_BYTES: usize =
    iris_core_domain::memory_budget::DEFAULT_MEMORY_BUDGET_BYTES;
const BYTES_PER_MIB: u64 = 1024 * 1024;
const MIN_MEMORY_BUDGET_MIB: u64 = 64;
const MAX_MEMORY_BUDGET_MIB: u64 = 4_096;

pub fn thumbnail_cache_limit_input_from_bytes(bytes: usize) -> String {
    (bytes as u64 / BYTES_PER_MIB).to_string()
}

pub fn parse_thumbnail_cache_limit_input_mib(input: &str) -> Option<usize> {
    let trimmed = input.trim();
    let mib: u64 = trimmed.parse().ok()?;
    usize::try_from(mib.saturating_mul(BYTES_PER_MIB)).ok()
}

pub fn memory_budget_limit_input_from_bytes(bytes: usize) -> String {
    (bytes as u64 / BYTES_PER_MIB).to_string()
}

pub fn parse_memory_budget_limit_input_mib(input: &str) -> Option<usize> {
    let mib: u64 = input.trim().parse().ok()?;
    if !(MIN_MEMORY_BUDGET_MIB..=MAX_MEMORY_BUDGET_MIB).contains(&mib) {
        return None;
    }
    usize::try_from(mib.saturating_mul(BYTES_PER_MIB)).ok()
}

/// Shallow folder statistics (direct children only).
#[derive(Clone, Debug)]
pub struct FolderStats {
    pub file_count: usize,
    pub dir_count: usize,
    pub total_size: u64,
}

/// Cheap stamp that detects content changes at the same path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryCacheStamp {
    pub stable_id: String,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,
}

impl EntryCacheStamp {
    pub fn from_entry(entry: &ListEntry) -> Self {
        Self {
            stable_id: entry.object_ref.key.stable_id.0.clone(),
            size: entry.size,
            mtime: entry.mtime,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ThumbnailCacheKey {
    pub path: String,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,
}

impl ThumbnailCacheKey {
    pub fn from_entry(entry: &ListEntry) -> Self {
        Self {
            path: entry.object_ref.display_path.clone(),
            size: entry.size,
            mtime: entry.mtime,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

#[derive(Clone, Debug)]
pub struct QueuedThumbnailRequest {
    pub object_key: ObjectKey,
    pub backend_type: String,
    pub priority: RequestPriority,
}

#[derive(Clone, Debug)]
pub enum PreviewData {
    Image {
        handle: Option<iced_image::Handle>,
        width: Option<u32>,
        height: Option<u32>,
        media: MediaMetadata,
        exif_data: Vec<(String, String)>,
    },
    Video {
        thumbnail: Option<iced_image::Handle>,
        duration_secs: Option<u64>,
    },
    Pdf {
        thumbnail: Option<iced_image::Handle>,
        page_count: Option<u32>,
    },
    Text {
        content: String,
    },
    Unknown {
        file_type: String,
        size: u64,
    },
}

#[derive(Debug)]
pub struct CachedThumbnail {
    pub handle: iced_image::Handle,
    pub memory_len: usize,
    pub(crate) _memory_permit: MemoryBudgetPermit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ThumbnailCacheLruNode {
    previous: Option<ThumbnailCacheKey>,
    next: Option<ThumbnailCacheKey>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThumbnailCacheLru {
    head: Option<ThumbnailCacheKey>,
    tail: Option<ThumbnailCacheKey>,
    links: HashMap<ThumbnailCacheKey, ThumbnailCacheLruNode>,
}

impl ThumbnailCacheLru {
    pub fn clear(&mut self) {
        self.head = None;
        self.tail = None;
        self.links.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn touch(&mut self, key: ThumbnailCacheKey) {
        if self.links.contains_key(&key) {
            self.detach(&key);
        }
        self.push_back_detached(key);
    }

    pub fn remove(&mut self, key: &ThumbnailCacheKey) -> bool {
        if !self.links.contains_key(key) {
            return false;
        }
        self.detach(key);
        self.links.remove(key).is_some()
    }

    pub fn pop_front(&mut self) -> Option<ThumbnailCacheKey> {
        let key = self.head.clone()?;
        self.remove(&key);
        Some(key)
    }

    pub fn retain<F>(&mut self, mut keep: F)
    where
        F: FnMut(&ThumbnailCacheKey) -> bool,
    {
        let keys = self.keys().collect::<Vec<_>>();
        for key in keys {
            if !keep(&key) {
                self.remove(&key);
            }
        }
    }

    pub fn keys(&self) -> ThumbnailCacheLruKeys<'_> {
        ThumbnailCacheLruKeys {
            lru: self,
            next: self.head.as_ref(),
        }
    }

    fn push_back_detached(&mut self, key: ThumbnailCacheKey) {
        let previous = self.tail.clone();
        if let Some(previous_key) = previous.as_ref() {
            if let Some(previous_node) = self.links.get_mut(previous_key) {
                previous_node.next = Some(key.clone());
            }
        }
        self.links.insert(
            key.clone(),
            ThumbnailCacheLruNode {
                previous,
                next: None,
            },
        );
        if self.head.is_none() {
            self.head = Some(key.clone());
        }
        self.tail = Some(key);
    }

    fn detach(&mut self, key: &ThumbnailCacheKey) {
        let Some(node) = self.links.get(key).cloned() else {
            return;
        };

        match node.previous.as_ref() {
            Some(previous_key) => {
                if let Some(previous_node) = self.links.get_mut(previous_key) {
                    previous_node.next = node.next.clone();
                }
            }
            None => {
                self.head = node.next.clone();
            }
        }

        match node.next.as_ref() {
            Some(next_key) => {
                if let Some(next_node) = self.links.get_mut(next_key) {
                    next_node.previous = node.previous.clone();
                }
            }
            None => {
                self.tail = node.previous;
            }
        }
    }
}

pub struct ThumbnailCacheLruKeys<'a> {
    lru: &'a ThumbnailCacheLru,
    next: Option<&'a ThumbnailCacheKey>,
}

impl Iterator for ThumbnailCacheLruKeys<'_> {
    type Item = ThumbnailCacheKey;

    fn next(&mut self) -> Option<Self::Item> {
        let key = self.next?.clone();
        self.next = self.lru.links.get(&key).and_then(|node| node.next.as_ref());
        Some(key)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StorageUsage {
    pub thumbnail_bytes: u64,
    pub index_bytes: u64,
    pub tile_bytes: u64,
    pub log_bytes: u64,
    pub quarantined_bytes: u64,
}

impl StorageUsage {
    pub fn total_bytes(&self) -> u64 {
        self.thumbnail_bytes
            .saturating_add(self.index_bytes)
            .saturating_add(self.tile_bytes)
            .saturating_add(self.log_bytes)
            .saturating_add(self.quarantined_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_memory_budget_input_enforces_supported_range() {
        assert_eq!(
            parse_memory_budget_limit_input_mib("256"),
            Some(256 * 1024 * 1024)
        );
        assert!(parse_memory_budget_limit_input_mib("63").is_none());
        assert!(parse_memory_budget_limit_input_mib("4097").is_none());
        assert!(parse_memory_budget_limit_input_mib("invalid").is_none());
    }
}
