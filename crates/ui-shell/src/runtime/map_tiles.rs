use super::*;
use crate::views::filter::map_panel::{
    osm_tile_url, tile_cache_relative_path, TileCoord, DEFAULT_TILE_CACHE_LIMIT_BYTES,
};

const MAP_TILE_USER_AGENT: &str = concat!("Iris/", env!("CARGO_PKG_VERSION"));
const MAX_TILE_BYTES: usize = 512 * 1024;
const MAP_TILE_FETCH_LOCK_COUNT: usize = 32;
const MAP_TILE_CACHE_MAINTENANCE_INTERVAL: u64 = 32;
const MAP_TILE_TOUCH_INTERVAL: StdDuration = StdDuration::from_secs(60 * 60);
static MAP_TILE_FETCH_LOCKS: OnceLock<Vec<tokio::sync::Mutex<()>>> = OnceLock::new();

pub(super) fn build_map_tile_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .tcp_nodelay(true)
        .connect_timeout(StdDuration::from_secs(5))
        .timeout(StdDuration::from_secs(15))
        .pool_idle_timeout(StdDuration::from_secs(90))
        .pool_max_idle_per_host(8)
        .build()
        .expect("failed to build map tile HTTP client")
}

impl AppRuntime {
    pub fn map_tile_cache_path(&self, tile: TileCoord) -> PathBuf {
        self.tile_cache_dir.join(tile_cache_relative_path(tile))
    }

    pub fn cached_map_tile(&self, tile: TileCoord) -> Option<Vec<u8>> {
        let path = self.map_tile_cache_path(tile);
        let metadata = std::fs::metadata(&path).ok()?;
        if metadata.len() > MAX_TILE_BYTES as u64 {
            let _ = std::fs::remove_file(path);
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        if !is_png(&bytes) {
            let _ = std::fs::remove_file(path);
            return None;
        }
        touch_map_tile_file(&path);
        Some(bytes)
    }

    pub async fn fetch_map_tile(&self, tile: TileCoord) -> Result<Vec<u8>, String> {
        if let Some(bytes) = self.cached_map_tile(tile) {
            return Ok(bytes);
        }

        // Separate panes may request the same tile concurrently. A small striped lock table
        // coalesces those misses without retaining one lock per coordinate forever.
        let _fetch_guard = map_tile_fetch_lock(tile).lock().await;
        if let Some(bytes) = self.cached_map_tile(tile) {
            return Ok(bytes);
        }

        let mut response = self
            .map_tile_http_client
            .get(osm_tile_url(tile))
            .header(reqwest::header::USER_AGENT, MAP_TILE_USER_AGENT)
            .send()
            .await
            .map_err(|error| format!("map tile request failed: {error}"))?
            .error_for_status()
            .map_err(|error| format!("map tile request failed: {error}"))?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !content_type.starts_with("image/png") {
            return Err(format!("map tile response is not png: {content_type}"));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_TILE_BYTES as u64)
        {
            return Err("map tile response exceeds size limit".to_string());
        }

        let mut bytes = Vec::with_capacity(
            response
                .content_length()
                .unwrap_or(32 * 1024)
                .min(MAX_TILE_BYTES as u64) as usize,
        );
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("map tile read failed: {error}"))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_TILE_BYTES {
                return Err("map tile response exceeds size limit".to_string());
            }
            bytes.extend_from_slice(&chunk);
        }
        if !is_png(&bytes) {
            return Err("map tile response has an invalid PNG signature".to_string());
        }
        self.write_map_tile(tile, &bytes)?;
        Ok(bytes)
    }

    fn write_map_tile(&self, tile: TileCoord, bytes: &[u8]) -> Result<(), String> {
        let path = self.map_tile_cache_path(tile);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let tmp = path.with_extension("png.tmp");
        std::fs::write(&tmp, bytes).map_err(|error| error.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|error| error.to_string())?;
        let writes = self.map_tile_cache_writes.fetch_add(1, Ordering::Relaxed);
        if writes == 0 || (writes + 1).is_multiple_of(MAP_TILE_CACHE_MAINTENANCE_INTERVAL) {
            evict_map_tile_cache_dir(&self.tile_cache_dir, DEFAULT_TILE_CACHE_LIMIT_BYTES)?;
        }
        Ok(())
    }
}

fn map_tile_fetch_lock(tile: TileCoord) -> &'static tokio::sync::Mutex<()> {
    let locks = MAP_TILE_FETCH_LOCKS.get_or_init(|| {
        (0..MAP_TILE_FETCH_LOCK_COUNT)
            .map(|_| tokio::sync::Mutex::new(()))
            .collect()
    });
    let hash = usize::from(tile.z)
        .wrapping_mul(31)
        .wrapping_add(tile.x as usize)
        .wrapping_mul(31)
        .wrapping_add(tile.y as usize);
    &locks[hash % locks.len()]
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn evict_map_tile_cache_dir(dir: &Path, max_bytes: u64) -> Result<(), String> {
    let mut files = Vec::new();
    collect_map_tile_files(dir, &mut files)?;
    let mut total = files.iter().map(|file| file.bytes).sum::<u64>();
    if total <= max_bytes {
        return Ok(());
    }

    files.sort_by_key(|file| file.modified);
    for file in files {
        if total <= max_bytes {
            break;
        }
        match std::fs::remove_file(&file.path) {
            Ok(()) => total = total.saturating_sub(file.bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                total = total.saturating_sub(file.bytes);
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn touch_map_tile_file(path: &Path) {
    if path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age < MAP_TILE_TOUCH_INTERVAL)
    {
        return;
    }
    let Ok(file) = std::fs::File::options().write(true).open(path) else {
        return;
    };
    let _ = file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now()));
}

#[derive(Debug)]
struct MapTileFile {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
}

fn collect_map_tile_files(dir: &Path, files: &mut Vec<MapTileFile>) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        if metadata.is_dir() {
            collect_map_tile_files(&path, files)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some("png") {
            files.push(MapTileFile {
                path,
                bytes: metadata.len(),
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_signature_validation_rejects_truncated_or_foreign_data() {
        assert!(is_png(b"\x89PNG\r\n\x1a\nrest"));
        assert!(!is_png(b"\x89PNG"));
        assert!(!is_png(b"<html>error</html>"));
    }

    #[test]
    fn identical_tiles_share_the_same_fetch_lock() {
        let tile = TileCoord {
            z: 8,
            x: 227,
            y: 100,
        };
        assert!(std::ptr::eq(
            map_tile_fetch_lock(tile),
            map_tile_fetch_lock(tile)
        ));
    }

    #[test]
    fn map_tile_cache_path_uses_slippy_layout() {
        let runtime = AppRuntime::initialize();
        let path = runtime.map_tile_cache_path(TileCoord {
            z: 8,
            x: 227,
            y: 100,
        });
        assert!(path.ends_with(Path::new("map-tiles").join("8").join("227").join("100.png")));
    }

    #[test]
    fn tile_cache_eviction_removes_oldest_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let old = temp.path().join("8/1/1.png");
        let new = temp.path().join("8/1/2.png");
        std::fs::create_dir_all(old.parent().unwrap()).expect("create old parent");
        std::fs::create_dir_all(new.parent().unwrap()).expect("create new parent");
        std::fs::write(&old, vec![1; 8]).expect("write old");
        std::fs::write(&new, vec![2; 8]).expect("write new");
        // Explicit times: file systems stamp writes with a coarse clock, so back-to-back writes
        // can share an mtime and make the eviction order depend on directory order.
        for (path, secs) in [(&old, 1_000), (&new, 2_000)] {
            std::fs::File::options()
                .write(true)
                .open(path)
                .expect("open tile")
                .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs))
                .expect("set tile time");
        }

        evict_map_tile_cache_dir(temp.path(), 8).expect("evict");

        assert!(!old.exists());
        assert!(new.exists());
    }

    #[test]
    fn touch_map_tile_file_refreshes_lru_timestamp() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("8/227/100.png");
        std::fs::create_dir_all(path.parent().unwrap()).expect("create parent");
        std::fs::write(&path, [1, 2, 3]).expect("write tile");
        let old_time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open tile")
            .set_times(std::fs::FileTimes::new().set_modified(old_time))
            .expect("set old time");

        touch_map_tile_file(&path);

        let refreshed = path
            .metadata()
            .expect("metadata")
            .modified()
            .expect("mtime");
        assert!(refreshed > old_time);
    }
}
