use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const ENCRYPTED_MAGIC: &[u8; 8] = b"IRISCCH1";
const DEFAULT_MAX_ENTRIES: usize = 512;
const DEFAULT_MAX_BYTES: u64 = 256 * 1024 * 1024;
static CACHE_INSTANCE_COUNTER: AtomicU64 = AtomicU64::new(1);
static CACHE_WRITE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
struct CacheEntry {
    path: PathBuf,
    byte_size: u64,
}

#[derive(Debug, Default)]
struct CacheState {
    entries: HashMap<String, CacheEntry>,
    lru: VecDeque<String>,
    total_bytes: u64,
}

/// Disposable per-session cache in the OS temp directory (thumbnails near the viewport).
/// Not kept across restarts; data is always encrypted with a per-session key.
pub struct SessionCache {
    base_dir: PathBuf,
    state: Mutex<CacheState>,
    encryption_key: [u8; 32],
    max_entries: usize,
    max_bytes: u64,
}

impl SessionCache {
    pub fn new() -> Result<Self, std::io::Error> {
        let instance = CACHE_INSTANCE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let base_dir = std::env::temp_dir()
            .join("iris-session-cache")
            .join(format!("{}-{instance}", std::process::id()));
        Self::with_base_dir(base_dir)
    }

    fn with_base_dir(base_dir: PathBuf) -> Result<Self, std::io::Error> {
        Self::with_limits(base_dir, DEFAULT_MAX_ENTRIES, DEFAULT_MAX_BYTES)
    }

    fn with_limits(
        base_dir: PathBuf,
        max_entries: usize,
        max_bytes: u64,
    ) -> Result<Self, std::io::Error> {
        std::fs::create_dir_all(&base_dir)?;
        let mut encryption_key = [0u8; 32];
        SystemRandom::new()
            .fill(&mut encryption_key)
            .map_err(|_| std::io::Error::other("failed to generate session cache key"))?;
        Ok(Self {
            base_dir,
            state: Mutex::new(CacheState::default()),
            encryption_key,
            max_entries,
            max_bytes,
        })
    }

    pub fn put(&self, key: &str, data: &[u8]) -> Result<(), std::io::Error> {
        let bytes = self.encrypt(data)?;
        let byte_size = bytes.len() as u64;
        if self.max_entries == 0 || byte_size > self.max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session cache entry exceeds configured capacity",
            ));
        }

        let file_name = format!("{}.cache", cache_key_digest(key));
        let path = self.base_dir.join(&file_name);
        let write_id = CACHE_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary_path = self.base_dir.join(format!(".{file_name}.{write_id}.tmp"));

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::write(&temporary_path, bytes)?;
        if let Err(rename_error) = std::fs::rename(&temporary_path, &path) {
            // Unix replaces atomically. Platforms that reject replacing an existing file
            // fall back to remove + rename while the cache index remains locked.
            if !path.exists() {
                let _ = std::fs::remove_file(&temporary_path);
                return Err(rename_error);
            }
            if let Err(remove_error) = std::fs::remove_file(&path) {
                let _ = std::fs::remove_file(&temporary_path);
                return Err(remove_error);
            }
            if let Err(error) = std::fs::rename(&temporary_path, &path) {
                let _ = std::fs::remove_file(&temporary_path);
                return Err(error);
            }
        }

        remove_from_state(&mut state, key);
        state
            .entries
            .insert(key.to_string(), CacheEntry { path, byte_size });
        state.total_bytes = state.total_bytes.saturating_add(byte_size);
        touch_lru(&mut state, key);
        evict_to_limits(&mut state, self.max_entries, self.max_bytes);
        Ok(())
    }

    pub fn read(&self, key: &str) -> Result<Vec<u8>, std::io::Error> {
        let entry = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let entry = state.entries.get(key).cloned().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("session cache entry not found: {}", key),
                )
            })?;
            touch_lru(&mut state, key);
            entry
        };
        let bytes = match std::fs::read(&entry.path) {
            Ok(bytes) => bytes,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::NotFound {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    remove_from_state(&mut state, key);
                }
                return Err(error);
            }
        };
        self.decrypt(&bytes)
    }

    pub fn remove(&self, key: &str) -> Result<(), std::io::Error> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = remove_from_state(&mut state, key) {
            if entry.path.exists() {
                std::fs::remove_file(&entry.path)?;
            }
        }
        Ok(())
    }

    pub fn remove_prefix(&self, prefix: &str) -> Result<usize, std::io::Error> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let keys = state
            .entries
            .keys()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();

        let mut removed = 0usize;
        for key in keys {
            if let Some(entry) = remove_from_state(&mut state, &key) {
                if entry.path.exists() {
                    std::fs::remove_file(&entry.path)?;
                }
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn clear(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        for entry in state.entries.values() {
            if entry.path.exists() {
                let _ = std::fs::remove_file(&entry.path);
            }
        }
        *state = CacheState::default();
    }

    fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
        let mut nonce_bytes = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| std::io::Error::other("failed to generate cache nonce"))?;
        let key = self.aead_key()?;
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);
        let mut sealed = data.to_vec();
        key.seal_in_place_append_tag(nonce, Aad::empty(), &mut sealed)
            .map_err(|_| std::io::Error::other("failed to encrypt cache data"))?;

        let mut out = Vec::with_capacity(ENCRYPTED_MAGIC.len() + nonce_bytes.len() + sealed.len());
        out.extend_from_slice(ENCRYPTED_MAGIC);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
        if data.len() < ENCRYPTED_MAGIC.len() + 12
            || &data[..ENCRYPTED_MAGIC.len()] != ENCRYPTED_MAGIC
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid encrypted session cache payload",
            ));
        }

        let mut nonce_bytes = [0u8; 12];
        nonce_bytes.copy_from_slice(&data[ENCRYPTED_MAGIC.len()..ENCRYPTED_MAGIC.len() + 12]);
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);
        let key = self.aead_key()?;
        let mut ciphertext = data[ENCRYPTED_MAGIC.len() + 12..].to_vec();
        let plaintext = key
            .open_in_place(nonce, Aad::empty(), &mut ciphertext)
            .map_err(|_| std::io::Error::other("failed to decrypt cache data"))?;
        Ok(plaintext.to_vec())
    }

    fn aead_key(&self) -> Result<LessSafeKey, std::io::Error> {
        let key = UnboundKey::new(&CHACHA20_POLY1305, &self.encryption_key)
            .map_err(|_| std::io::Error::other("invalid session cache key"))?;
        Ok(LessSafeKey::new(key))
    }
}

impl Drop for SessionCache {
    fn drop(&mut self) {
        self.clear();
        let _ = std::fs::remove_dir(&self.base_dir);
    }
}

fn touch_lru(state: &mut CacheState, key: &str) {
    state.lru.retain(|candidate| candidate != key);
    state.lru.push_back(key.to_string());
}

fn remove_from_state(state: &mut CacheState, key: &str) -> Option<CacheEntry> {
    let entry = state.entries.remove(key)?;
    state.total_bytes = state.total_bytes.saturating_sub(entry.byte_size);
    state.lru.retain(|candidate| candidate != key);
    Some(entry)
}

fn evict_to_limits(state: &mut CacheState, max_entries: usize, max_bytes: u64) {
    while state.entries.len() > max_entries || state.total_bytes > max_bytes {
        let Some(key) = state.lru.pop_front() else {
            break;
        };
        let Some(entry) = state.entries.remove(&key) else {
            continue;
        };
        state.total_bytes = state.total_bytes.saturating_sub(entry.byte_size);
        let _ = std::fs::remove_file(entry.path);
    }
}

/// Stable, collision-resistant filename for arbitrary cache keys.
fn cache_key_digest(key: &str) -> String {
    digest(&SHA256, key.as_bytes()).as_ref()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cache_path(dir: &TempDir, key: &str) -> PathBuf {
        dir.path().join(format!("{}.cache", cache_key_digest(key)))
    }

    #[test]
    fn test_session_cache_put_and_read() {
        let dir = TempDir::new().unwrap();
        let cache = SessionCache::with_base_dir(dir.path().to_path_buf()).unwrap();

        cache.put("test-key", b"hello world").unwrap();
        let path = cache_path(&dir, "test-key");
        assert!(path.exists());

        let data = cache.read("test-key").unwrap();
        assert_eq!(data, b"hello world");
        assert_ne!(std::fs::read(path).unwrap(), b"hello world");
    }

    #[test]
    fn test_session_cache_remove() {
        let dir = TempDir::new().unwrap();
        let cache = SessionCache::with_base_dir(dir.path().to_path_buf()).unwrap();

        cache.put("remove-me", b"data").unwrap();
        let path = cache_path(&dir, "remove-me");
        assert!(path.exists());

        cache.remove("remove-me").unwrap();
        assert!(!path.exists());
        assert_eq!(
            cache.read("remove-me").unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn test_session_cache_remove_prefix() {
        let dir = TempDir::new().unwrap();
        let cache = SessionCache::with_base_dir(dir.path().to_path_buf()).unwrap();

        cache.put("thumb:local:file1:128", b"a").unwrap();
        cache.put("thumb:local:file1:256", b"b").unwrap();
        cache.put("thumb:local:file2:128", b"c").unwrap();
        let a = cache_path(&dir, "thumb:local:file1:128");
        let b = cache_path(&dir, "thumb:local:file1:256");
        let c = cache_path(&dir, "thumb:local:file2:128");

        let removed = cache.remove_prefix("thumb:local:file1:").unwrap();

        assert_eq!(removed, 2);
        assert!(cache.read("thumb:local:file1:128").is_err());
        assert!(cache.read("thumb:local:file1:256").is_err());
        assert_eq!(cache.read("thumb:local:file2:128").unwrap(), b"c");
        assert!(!a.exists());
        assert!(!b.exists());
        assert!(c.exists());
    }

    #[test]
    fn session_cache_evicts_least_recently_used_entry() {
        let dir = TempDir::new().unwrap();
        let cache = SessionCache::with_limits(dir.path().to_path_buf(), 2, 1024).unwrap();

        cache.put("first", b"1").unwrap();
        cache.put("second", b"2").unwrap();
        let first = cache_path(&dir, "first");
        let second = cache_path(&dir, "second");
        assert_eq!(cache.read("first").unwrap(), b"1");
        cache.put("third", b"3").unwrap();
        let third = cache_path(&dir, "third");

        assert!(first.exists());
        assert!(!second.exists());
        assert!(third.exists());
        assert_eq!(cache.read("first").unwrap(), b"1");
        assert!(cache.read("second").is_err());
        assert_eq!(cache.read("third").unwrap(), b"3");
    }

    #[test]
    fn session_cache_overwrite_replaces_data_and_updates_size() {
        let dir = TempDir::new().unwrap();
        let cache = SessionCache::with_limits(dir.path().to_path_buf(), 8, 77).unwrap();

        cache.put("same-key", b"1234").unwrap();
        let old_path = cache_path(&dir, "same-key");
        cache.put("same-key", b"12").unwrap();
        let new_path = cache_path(&dir, "same-key");
        cache.put("another", b"345").unwrap();

        assert_eq!(old_path, new_path);
        assert!(new_path.exists());
        assert_eq!(cache.read("same-key").unwrap(), b"12");
        assert_eq!(cache.read("another").unwrap(), b"345");
    }

    #[test]
    fn long_similar_keys_use_distinct_cache_files() {
        let dir = TempDir::new().unwrap();
        let cache = SessionCache::with_base_dir(dir.path().to_path_buf()).unwrap();
        let shared_prefix = "a".repeat(96);

        let first_key = format!("{shared_prefix}-first");
        let second_key = format!("{shared_prefix}-second");
        cache.put(&first_key, b"first").unwrap();
        cache.put(&second_key, b"second").unwrap();
        let first = cache_path(&dir, &first_key);
        let second = cache_path(&dir, &second_key);

        assert_ne!(first, second);
        assert_eq!(cache.read(&first_key).unwrap(), b"first");
        assert_eq!(cache.read(&second_key).unwrap(), b"second");
    }

    #[test]
    fn missing_cache_file_is_pruned_from_index() {
        let dir = TempDir::new().unwrap();
        let cache = SessionCache::with_base_dir(dir.path().to_path_buf()).unwrap();
        cache.put("missing", b"data").unwrap();
        let path = cache_path(&dir, "missing");
        std::fs::remove_file(path).unwrap();

        assert_eq!(
            cache.read("missing").unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        let state = cache
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(!state.entries.contains_key("missing"));
    }
}
